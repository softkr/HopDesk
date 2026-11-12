#!/bin/sh
# One-time: creates the self-signed certificate that signs HopDesk.app on every release.
# Signing every build with the same certificate keeps the app's identity stable, so the
# Keychain does not ask again for saved passwords after an update.
# Usage: scripts/make-signing-cert.sh [output-dir]   (default: ~/.hopdesk-signing)
# Writes <output-dir>/hopdesk-signing.p12 and its password; keep both, outside the repo.
# Losing them means the next certificate makes users allow Keychain access once more.
set -eu

NAME="HopDesk Self-Signed"
VALID_DAYS=7300 # 20 years: the certificate's leaf hash is what users' Keychains trust
OUT=${1:-"$HOME/.hopdesk-signing"}
P12="$OUT/hopdesk-signing.p12"

if [ -e "$P12" ]; then
    echo "$P12 already exists; refusing to replace the signing certificate" >&2
    exit 1
fi
mkdir -p "$OUT"
chmod 700 "$OUT"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
umask 077

cat > "$TMP/cert.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
x509_extensions = ext
[dn]
CN = $NAME
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF

openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days "$VALID_DAYS" \
    -config "$TMP/cert.cnf" -keyout "$TMP/key.pem" -out "$TMP/cert.pem"

PASSWORD=$(openssl rand -hex 24)
# SHA1/3DES encryption: the only PKCS#12 format macOS `security import` reads.
openssl pkcs12 -export -name "$NAME" -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
    -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1 \
    -passout "pass:$PASSWORD" -out "$P12"
printf '%s\n' "$PASSWORD" > "$OUT/hopdesk-signing.password"

echo "created $P12"
echo "Add it to the repository's Actions secrets:"
echo "  base64 -i '$P12' | gh secret set MACOS_SIGN_P12"
echo "  gh secret set MACOS_SIGN_P12_PASSWORD < '$OUT/hopdesk-signing.password'"
