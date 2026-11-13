#!/bin/sh
# Prints the codesigning identity to sign local builds with, preparing it on first use.
#
# With HOPDESK_SIGN_IDENTITY already set (as the release workflow does), that wins untouched.
# Otherwise, when scripts/make-signing-cert.sh's certificate exists in ~/.hopdesk-signing, the
# certificate is imported into the login keychain and trusted for code signing once, so every
# later install signs with the same identity: the Keychain then keeps trusting saved passwords
# across rebuilds and updates instead of asking again after each one.
#
# Prints nothing (ad-hoc signing, the old behavior) when there is no certificate to set up.
set -eu

NAME="HopDesk Self-Signed"
SIGNING="$HOME/.hopdesk-signing"
P12="$SIGNING/hopdesk-signing.p12"

if [ -n "${HOPDESK_SIGN_IDENTITY:-}" ]; then
    printf '%s\n' "$HOPDESK_SIGN_IDENTITY"
    exit 0
fi

identity_in_keychain() {
    security find-identity -v -p codesigning 2>/dev/null | grep -Fq "\"$NAME\""
}

if ! identity_in_keychain && [ -e "$P12" ]; then
    PASSWORD=$(cat "$SIGNING/hopdesk-signing.password")
    # A duplicate import (certificate present, trust missing) is not an error here.
    security import "$P12" -P "$PASSWORD" -k "$HOME/Library/Keychains/login.keychain-db" \
        -T /usr/bin/codesign >/dev/null 2>&1 || true
    CERT=$(mktemp)
    trap 'rm -f "$CERT"' EXIT
    if openssl pkcs12 -in "$P12" -passin "pass:$PASSWORD" -nokeys -nodes -out "$CERT" >/dev/null 2>&1 \
        && security add-trusted-cert -r trustRoot -p codeSign \
            -k "$HOME/Library/Keychains/login.keychain-db" "$CERT" >/dev/null 2>&1 \
        && identity_in_keychain; then
        echo "prepared the $NAME signing identity" >&2
    else
        echo "warning: could not prepare $NAME; falling back to ad-hoc signing" >&2
    fi
fi

if identity_in_keychain; then
    printf '%s\n' "$NAME"
else
    [ -e "$P12" ] || echo "hint: scripts/make-signing-cert.sh creates the signing certificate" >&2
fi
