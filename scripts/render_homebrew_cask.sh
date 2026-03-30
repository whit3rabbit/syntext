#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 5 ]; then
  echo "usage: $0 TEMPLATE OUTPUT VERSION ARM_SHA256 X86_SHA256" >&2
  exit 1
fi

template=$1
output=$2
version=$3
arm_sha=$4
x86_sha=$5

mkdir -p "$(dirname "$output")"

sed \
  -e "s|__VERSION__|${version}|g" \
  -e "s|__ARM_SHA256__|${arm_sha}|g" \
  -e "s|__X86_SHA256__|${x86_sha}|g" \
  "$template" > "$output"
