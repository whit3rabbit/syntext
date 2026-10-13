#!/usr/bin/env python3
"""Generate eval/sets/multihop.json: multi-hop questions over a fictional
world of people and companies, with lexical decoys.

Deterministic (fixed seed). Facts are spread over company profiles, person
profiles, standalone notes, and filler; company names share tokens on
purpose ("Meridian Labs", "Meridian Robotics") so lexical matching alone is
confused. Every question names the passages its answer depends on.

    python3 context/eval/gen_multihop.py > context/eval/sets/multihop.json
"""

import json
import random

rng = random.Random(20261013)

FIRST = ["Alice", "Bruno", "Chiara", "Dmitri", "Elena", "Farid", "Grace", "Hiro",
         "Ines", "Jonah", "Keiko", "Lars", "Maya", "Nikhil", "Olga", "Pedro",
         "Quinn", "Rosa", "Samir", "Tessa", "Umar", "Vera", "Wes", "Xenia",
         "Yusuf", "Zoe", "Amara", "Bjorn", "Camila", "Dario", "Esther", "Felix"]
LAST = ["Okafor", "Lindqvist", "Moreau", "Tanaka", "Haddad", "Novak", "Ferreira",
        "Kowalski", "Brennan", "Castillo", "Duarte", "Eriksen", "Gallo", "Hughes",
        "Ivanova", "Jensen", "Kapoor", "Laurent", "Mendez", "Nakamura", "Oduya",
        "Petrov", "Quiroga", "Rahman", "Sato", "Torres", "Ueda", "Varga", "Weber"]
PREFIX = ["Meridian", "Northstar", "Bluefin", "Copperleaf", "Halcyon", "Ironwood",
          "Juniper", "Kestrel", "Lumen", "Marrow"]
SUFFIX = ["Labs", "Systems", "Robotics", "Analytics", "Foods", "Logistics"]
CITIES = ["Lisbon", "Osaka", "Denver", "Tallinn", "Montreal", "Nairobi", "Porto",
          "Krakow", "Austin", "Ghent", "Perth", "Leipzig", "Valencia", "Tucson"]
PRODUCTS = ["Lattice router", "Quill editor", "Harbor scheduler", "Ember thermostat",
            "Tidal pump", "Atlas tracker", "Sable drone", "Orbit kiosk", "Vesper lamp",
            "Pioneer scale", "Cinder oven", "Drift sensor", "Fathom sonar", "Gale turbine"]
ROLES = ["data engineer", "product manager", "designer", "staff accountant",
         "support lead", "site reliability engineer", "recruiter", "analyst"]
FILLER = [
    "The office hosts a quarterly hackathon open to every team.",
    "Its annual report highlights steady growth in recurring revenue.",
    "The cafeteria menu changes every Monday.",
    "Employees can join a weekly running club.",
    "The board meets twice a year to review the budget.",
    "A new parking garage opened next to the main building.",
    "The company sponsors a regional science fair for students.",
    "Most teams use a shared calendar for release planning.",
]
NOISE = [
    "The company founded a mentoring program and the employer survey was acquired by a vendor.",
    "Our employer headquartered the offsite in a new city this year, and the founder spoke.",
    "Which company makes the best coffee is a long-running debate in the office.",
    "The team acquired new laptops; the founder of the club ordered them.",
    "An employer survey asked where people would like the company headquartered.",
    "The founded-year field in the CRM is often blank for acquired accounts.",
    "Product teams that make hardware report to the city office.",
    "A company newsletter covered who founded the book club and who acquired the trophy.",
]


def pick(seq):
    return seq[rng.randrange(len(seq))]


def build_world():
    names = [f"{p} {s}" for p in PREFIX for s in SUFFIX]
    rng.shuffle(names)
    companies = names[:30]
    people = [f"{f} {l}" for f in FIRST for l in LAST]
    rng.shuffle(people)
    people_iter = iter(people)
    world = {"companies": {}, "people": {}}
    products = PRODUCTS[:]
    rng.shuffle(products)
    for i, c in enumerate(companies):
        world["companies"][c] = {
            "founder": next(people_iter),
            "founded": rng.randrange(1975, 2012),
            "hq": pick(CITIES),
            "product": products[i] if i < len(products) else None,
            # Acquired by an earlier company, so acquisitions form a DAG.
            "acquirer": companies[rng.randrange(i)] if i > 3 and rng.random() < 0.55 else None,
            "acquired_year": rng.randrange(2012, 2024),
        }
    for _ in range(40):
        p = next(people_iter)
        world["people"][p] = {"employer": pick(companies), "role": pick(ROLES),
                              "joined": rng.randrange(2005, 2024)}
    return world


def facts(world):
    """Every fact as (key, subject, sentence)."""
    out = []
    for c, d in world["companies"].items():
        f, y = d["founder"], d["founded"]
        out.append((("founded", c), c, pick([
            f"{c} was founded by {f} in {y}.",
            f"{f} founded {c} in {y}.",
            f"{f} started {c} in {y} after leaving university.",
        ])))
        out.append((("hq", c), c, pick([
            f"{c} is headquartered in {d['hq']}.",
            f"{c} has its headquarters in {d['hq']}.",
            f"The head office of {c} is in {d['hq']}.",
        ])))
        if d["product"]:
            out.append((("makes", c), c, pick([
                f"{c} makes the {d['product']}.",
                f"The {d['product']} is made by {c}.",
                f"{c} designs and sells the {d['product']}.",
            ])))
        if d["acquirer"]:
            a, y2 = d["acquirer"], d["acquired_year"]
            out.append((("acquired", c), a, pick([
                f"{a} acquired {c} in {y2}.",
                f"In {y2}, {a} bought {c}.",
                f"{c} was acquired by {a} in {y2}.",
            ])))
    for p, d in world["people"].items():
        c = d["employer"]
        out.append((("works_for", p), p, pick([
            f"{p} works for {c} as a {d['role']}.",
            f"{p} joined {c} in {d['joined']} as a {d['role']}.",
            f"{c} employs {p} as a {d['role']}.",
        ])))
    return out


def layout(world, all_facts):
    """Spread facts over documents; return docs and fact -> doc id."""
    docs, where = [], {}
    profiles = {}
    for key, subject, sentence in all_facts:
        r = rng.random()
        if r < 0.4:
            doc_id = f"fact-{len(docs):03d}"
            title = pick([f"Notes on {subject}", f"Memo {len(docs)}", f"Record {len(docs)}"])
            docs.append({"id": doc_id, "title": title, "text": f"# {title}\n\n{sentence}\n"})
            where[key] = (doc_id, sentence)
        else:
            profiles.setdefault(subject, []).append((key, sentence))
    for subject, items in profiles.items():
        doc_id = f"profile-{len(docs):03d}"
        body = []
        for key, sentence in items:
            body.append(sentence)
            if rng.random() < 0.6:
                body.append(pick(FILLER))
            where[key] = (doc_id, sentence)
        docs.append({"id": doc_id, "title": subject,
                     "text": f"# {subject}\n\n" + " ".join(body) + "\n"})
    for i, line in enumerate(NOISE * 4):
        docs.append({"id": f"noise-{i:02d}", "title": f"Bulletin {i}",
                     "text": f"# Bulletin {i}\n\n{line} {pick(FILLER)}\n"})
    rng.shuffle(docs)
    return docs, where


def support(where, key):
    doc_id, sentence = where[key]
    return {"doc": f"note:{doc_id}", "contains": sentence.rstrip(".")}


def questions(world, where):
    comps, people = world["companies"], world["people"]
    qs = []
    # 3 hops: person -> employer -> acquired company -> founder.
    for p, d in people.items():
        acquired = [c for c, x in comps.items() if x["acquirer"] == d["employer"]]
        if len(acquired) == 1:  # "the company acquired" must be unambiguous
            c2 = acquired[0]
            qs.append(("3-hop", f"Who founded the company acquired by {p}'s employer?",
                       [("works_for", p), ("acquired", c2), ("founded", c2)]))
    # 2 hops: person -> employer -> headquarters.
    for p, d in list(people.items())[:12]:
        qs.append(("2-hop", f"In which city is {p}'s employer headquartered?",
                   [("works_for", p), ("hq", d["employer"])]))
    # 2 hops: product -> maker -> founder.
    for c, x in comps.items():
        if x["product"]:
            qs.append(("2-hop", f"Who founded the company that makes the {x['product']}?",
                       [("makes", c), ("founded", c)]))
    # 2 hops: founder -> company -> acquirer.
    for c, x in comps.items():
        if x["acquirer"]:
            qs.append(("2-hop", f"Which company acquired the company founded by {x['founder']}?",
                       [("founded", c), ("acquired", c)]))
    # 3 hops: product -> maker -> acquirer -> headquarters.
    for c, x in comps.items():
        if x["product"] and x["acquirer"]:
            qs.append(("3-hop", f"Where is the company that acquired the maker of the "
                       f"{x['product']} headquartered?",
                       [("makes", c), ("acquired", c), ("hq", x["acquirer"])]))
    rng.shuffle(qs)
    three = [q for q in qs if q[0] == "3-hop"][:20]
    two = [q for q in qs if q[0] == "2-hop"][:20]
    chosen = sorted(three + two, key=lambda q: q[1])
    return [{"id": f"mh-{i + 1:03d}", "question": text, "tags": [hops],
             "supports": [support(where, k) for k in keys]}
            for i, (hops, text, keys) in enumerate(chosen)]


def main():
    world = build_world()
    all_facts = facts(world)
    docs, where = layout(world, all_facts)
    out = {
        "name": "multihop",
        "description": "Two- and three-hop questions over a fictional world of people and "
                       "companies; facts are spread over profiles and notes, among lexical decoys. "
                       "Generated by context/eval/gen_multihop.py.",
        "corpus": {"kind": "notes", "docs": docs},
        "questions": questions(world, where),
    }
    print(json.dumps(out, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
