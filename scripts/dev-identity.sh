#!/bin/bash
#
# Create the local code-signing identity that development builds are signed
# with, once, so that they stop being a different application every time.
#
# The Keychain remembers who is allowed to read an item by code signature. An
# ad-hoc signature is a hash of the binary, so every `cargo build` produces an
# application macOS has never seen before, the remembered permission does not
# match, and the "Tupli wants to use your confidential information" prompt comes
# back — not because anything is wrong, but because the app genuinely has a new
# identity. Signing with a certificate instead makes the identity the
# certificate, which outlives the build.
#
# This is not distribution signing and cannot become it: the certificate is
# self-signed and trusted only in this user's keychain, so it means nothing on
# any other machine. Releases are signed with a Developer ID and notarized.
#
# Run it once. It is idempotent; running it again is a no-op.
#
# Usage:
#   scripts/dev-identity.sh [--name NAME] [--remove]
#
# macOS will ask for the login password once, when the certificate is added to
# the trust settings. That is the prompt this script exists to stop having to
# answer.

set -euo pipefail

name="Tupli Development"
remove=0
while [ $# -gt 0 ]; do
  case $1 in
    --name) name=${2:?--name needs a value}; shift 2 ;;
    --name=*) name=${1#*=}; shift ;;
    --remove) remove=1; shift ;;
    -h|--help) sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "dev-identity: unknown argument: $1" >&2; exit 2 ;;
  esac
done

keychain="$HOME/Library/Keychains/login.keychain-db"

if [ "$remove" = 1 ]; then
  security delete-certificate -c "$name" -t "$keychain" 2>/dev/null \
    && echo "dev-identity: removed $name" \
    || echo "dev-identity: nothing named $name to remove"
  exit 0
fi

if security find-identity -v -p codesigning "$keychain" 2>/dev/null | grep -qF "$name"; then
  echo "dev-identity: $name already exists"
  exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# `codeSigning` in extendedKeyUsage is what makes `security find-identity
# -p codesigning` return it at all, and CA:true is what lets the chain
# terminate at the certificate itself rather than nowhere.
cat > "$work/openssl.cnf" <<CNF
[ req ]
distinguished_name = dn
x509_extensions    = ext
prompt             = no
[ dn ]
CN = $name
[ ext ]
basicConstraints       = critical,CA:true
keyUsage               = critical,digitalSignature
extendedKeyUsage       = critical,codeSigning
subjectKeyIdentifier   = hash
CNF

openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout "$work/key.pem" -out "$work/cert.pem" -config "$work/openssl.cnf" \
  >/dev/null 2>&1
openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" \
  -out "$work/identity.p12" -passout pass: -legacy >/dev/null 2>&1 \
  || openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" \
       -out "$work/identity.p12" -passout pass: >/dev/null 2>&1

# `-T /usr/bin/codesign` is the whole point of importing rather than adding:
# it puts codesign on the private key's own access list, so signing does not
# prompt either.
security import "$work/identity.p12" -k "$keychain" -P "" \
  -T /usr/bin/codesign -T /usr/bin/security >/dev/null

# Without a trust setting codesign refuses the certificate as untrusted for the
# purpose. `-p codeSign` grants it for that purpose and nothing else, and `-k`
# keeps it in this user's keychain rather than the system's, which is why this
# needs a password and not an administrator.
security add-trusted-cert -r trustRoot -p codeSign -k "$keychain" "$work/cert.pem"

security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
  -k "" "$keychain" >/dev/null 2>&1 || true

security find-identity -v -p codesigning "$keychain" | grep -F "$name" \
  || { echo "dev-identity: the identity was not created" >&2; exit 1; }
echo "dev-identity: signing development builds as $name"
