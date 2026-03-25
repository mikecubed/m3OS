#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/gen-secure-boot-keys.sh [output-dir]

Generate:
  - m3os.key (4096-bit RSA private key)
  - m3os.crt (self-signed X.509 certificate)

By default these files are written to the repository root so future
`cargo xtask sign` support can discover `./m3os.key` and `./m3os.crt`.
Pass an output directory to generate them elsewhere for testing.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

if [[ "$#" -gt 1 ]]; then
    usage >&2
    exit 1
fi

if ! command -v openssl >/dev/null 2>&1; then
    echo "error: openssl is required to generate Secure Boot keys" >&2
    exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
output_dir="${1:-$repo_root}"

umask 077

mkdir -p "$output_dir"

key_path="${output_dir}/m3os.key"
cert_path="${output_dir}/m3os.crt"

if [[ -e "$key_path" || -e "$cert_path" ]]; then
    echo "error: refusing to overwrite existing key material in ${output_dir}" >&2
    exit 1
fi

openssl req \
    -x509 \
    -newkey rsa:4096 \
    -keyout "$key_path" \
    -out "$cert_path" \
    -sha256 \
    -days 3650 \
    -nodes \
    -subj "/CN=m3os Secure Boot Key"

chmod 600 "$key_path"
chmod 644 "$cert_path"

cat <<EOF
Generated Secure Boot key material:
  key:  $key_path
  cert: $cert_path

Place these files at the repository root as ./m3os.key and ./m3os.crt for
future \`cargo xtask sign\` support.
EOF
