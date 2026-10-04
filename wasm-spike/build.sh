#!/usr/bin/env bash
# Builds the harness module and prints its sizes: the release cdylib for wasm32, its JS bindings,
# then a size pass. WASM_BINDGEN must match the wasm-bindgen crate the lockfile resolves.
# A C compiler that targets wasm32 builds SQLite: set CC_wasm32_unknown_unknown and
# AR_wasm32_unknown_unknown when the default one does not.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
target_dir="${CARGO_TARGET_DIR:-$here/../target}"
bindgen="${WASM_BINDGEN:-wasm-bindgen}"
opt="${WASM_OPT:-wasm-opt}"
out="$here/www/pkg"

cargo build --release --target wasm32-unknown-unknown -p anki-wasm-spike
raw="$target_dir/wasm32-unknown-unknown/release/anki_wasm_spike.wasm"

mkdir -p "$out"
"$bindgen" --target web --out-dir "$out" "$raw"
bound="$out/anki_wasm_spike_bg.wasm"
cp "$bound" "$out/unoptimised.wasm"
"$opt" -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
  --enable-mutable-globals --enable-reference-types --enable-multivalue \
  "$out/unoptimised.wasm" -o "$bound"

size() { printf '%-28s raw %10d  gzip-9 %10d\n' "$1" "$(wc -c < "$2")" "$(gzip -9 -c "$2" | wc -c)"; }
size "cargo cdylib" "$raw"
size "after wasm-bindgen" "$out/unoptimised.wasm"
size "after wasm-opt -Oz" "$bound"
size "JS bindings" "$out/anki_wasm_spike.js"
