# Install a released binary

Downloads carry a SHA-256 checksum and a Sigstore build-provenance
attestation. Verify both before you run the binary. The archives are
`.tar.xz`; substitute your platform's target triple for `$asset` below
(`aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-gnu`, or `x86_64-unknown-linux-gnu`).

```sh
asset="axiom-rules-engine-aarch64-apple-darwin.tar.xz"

# Download the archive and its checksum.
gh release download v0.1.0 \
  --repo TheAxiomFoundation/axiom-rules-engine \
  --pattern "$asset" \
  --pattern "$asset.sha256"

# Verify bytes against the published checksum.
sha256sum --check "$asset.sha256"          # Linux
# macOS: shasum -a 256 --check "$asset.sha256"

# Verify GitHub/Sigstore build provenance.
gh attestation verify "$asset" \
  --repo TheAxiomFoundation/axiom-rules-engine \
  --source-ref refs/tags/v0.1.0

# Only extract after both checks pass.
tar -xJf "$asset"
```

## Trusted rule content

The downloaded binary alone runs a compiled artifact or a self-contained JSON
`ExecutionRequest`. Compiling canonical `us:` imports from source additionally
requires a rulespec checkout (or a downloaded program-artifacts release),
passed with the **required, repeatable `--rulespec-root`** flag — one absolute
path per canonical country repo:

```sh
axiom-rules-engine compile-composed \
  --program /abs/path/to/program.yaml \
  --rulespec-root /abs/path/to/rulespec-us \
  --output compiled.json
```

[Artifact releases](https://github.com/TheAxiomFoundation/axiom-rules-engine/releases)
carry Sigstore attestations and the corpus releases they cite are
Ed25519-signed against the org trust root — so a downloaded artifact's
provenance is verifiable independently of where you got the binary.
