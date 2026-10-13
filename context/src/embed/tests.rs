use super::table::{sidecar_id, sidecar_key, sidecar_name, sidecar_segment, VectorTable};
use super::*;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

/// Deterministic pseudo-random vector.
fn vector(seed: u64, dims: usize) -> Vec<f32> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..dims)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % 2001) as f32 / 1000.0 - 1.0
        })
        .collect()
}

#[test]
fn quantized_similarity_tracks_cosine() {
    for seed in 0..50u64 {
        let (a, b) = (vector(seed, 384), vector(seed + 1000, 384));
        let mixed: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + 0.5 * y).collect();
        let (sa, qa) = quantize(&a).unwrap();
        let (sm, qm) = quantize(&mixed).unwrap();
        let approx = dot(&qa, &qm) as f32 * sa * sm;
        assert!(
            (approx - cosine(&a, &mixed)).abs() < 0.02,
            "{approx} vs {}",
            cosine(&a, &mixed)
        );
    }
    assert!(quantize(&[0.0; 8]).is_none());
    assert!(quantize(&[f32::NAN, 1.0]).is_none());
}

#[test]
fn tables_round_trip_and_reject_damage() {
    let mut t = VectorTable::empty("toy-v1", 8, 3);
    let (s, q) = quantize(&vector(7, 8)).unwrap();
    t.set(1, 42, s, &q);
    let bytes = t.encode();
    let back = VectorTable::decode(&bytes, "toy-v1", 3).unwrap();
    assert_eq!(back, t);
    assert_eq!(back.present(), 1);
    assert!(back.get(0).is_none());
    assert_eq!(back.get(1).unwrap().0, 42);
    assert_eq!(sidecar_id(&bytes).as_deref(), Some("toy-v1"));

    assert!(
        VectorTable::decode(&bytes, "other", 3).is_err(),
        "different embedder"
    );
    assert!(
        VectorTable::decode(&bytes, "toy-v1", 4).is_err(),
        "different segment"
    );
    for i in 0..bytes.len() {
        let mut bad = bytes.clone();
        bad[i] ^= 0x41;
        assert!(VectorTable::decode(&bad, "toy-v1", 3).is_err(), "byte {i}");
    }
    for len in 0..bytes.len() {
        assert!(VectorTable::decode(&bytes[..len], "toy-v1", 3).is_err());
    }
}

#[test]
fn sidecar_names_map_back_to_their_segment() {
    let key = embedder_key("toy-v1");
    let name = sidecar_name("seg-000000000003-1a2b3c4d5e6f.ctxs", &key);
    assert_eq!(name, format!("seg-000000000003-1a2b3c4d5e6f.{key}.vec"));
    assert_eq!(
        sidecar_segment(&name).as_deref(),
        Some("seg-000000000003-1a2b3c4d5e6f.ctxs")
    );
    assert_eq!(sidecar_key(&name), Some(key.as_str()));
    for bad in [
        "manifest.json",
        "seg-1.ctxs",
        "seg-1.xyz.vec",
        "x.0123456789abcdef.vec",
    ] {
        assert!(sidecar_segment(bad).is_none(), "{bad}");
    }
}

#[test]
fn embedded_text_carries_its_location() {
    let t = passage_text("docs/billing.md > Refunds", None, "  Refunds are issued.  ");
    assert_eq!(t, "docs/billing.md > Refunds\nRefunds are issued.");
    let t = passage_text("src/a.rs > impl A", Some("retry"), "fn retry() {}");
    assert!(t.starts_with("src/a.rs > impl A > retry\n"));
    let long = "x".repeat(10_000);
    assert!(passage_text("f", None, &long).len() < 2100);
    assert_ne!(text_hash("a"), text_hash("b"));
    assert_ne!(text_hash(""), 0);
}
