#!/bin/bash
# Create a local self-signed CODE-SIGNING certificate "Oxide Dev" (one time).
# Signing every build with the SAME identity keeps macOS TCC ("Allow access…")
# grants across updates — ad-hoc signatures change per build, so TCC re-asks.
set -euo pipefail

NAME="Oxide Dev"
if security find-identity -v -p codesigning | grep -q "$NAME"; then
  echo "✓ '$NAME' already exists"; exit 0
fi

TMP=$(mktemp -d)
cat > "$TMP/ext.cnf" <<'EOF'
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = Oxide Dev
[ext]
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
basicConstraints = critical,CA:false
EOF

openssl req -x509 -newkey rsa:2048 -days 3650 -nodes \
  -keyout "$TMP/key.pem" -out "$TMP/cert.pem" -config "$TMP/ext.cnf" >/dev/null 2>&1
openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
  -name "$NAME" -out "$TMP/oxide.p12" -passout pass:oxide >/dev/null 2>&1

security import "$TMP/oxide.p12" -k ~/Library/Keychains/login.keychain-db \
  -P oxide -T /usr/bin/codesign

# Trust it for code signing (asks for your password once).
sudo security add-trusted-cert -d -r trustRoot \
  -k /Library/Keychains/System.keychain "$TMP/cert.pem"

rm -rf "$TMP"
echo "✓ created + trusted '$NAME' — rebuild the dmg; future updates keep their permissions"
