# Install a released binary

Downloads carry a SHA-256 checksum and a Sigstore build-provenance
attestation. Verify both before you run the binary. The archives are
`.tar.xz`; substitute your platform's target triple for `$asset` below
(`aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-gnu`, or `x86_64-unknown-linux-gnu`).

```sh
asset="axiom-rules-engine-aarch64-apple-darwin.tar.xz"

# Download the archive and its checksum.
gh release download v0.1.1 \
  --repo TheAxiomFoundation/axiom-rules-engine \
  --pattern "$asset" \
  --pattern "$asset.sha256"

# Verify bytes against the published checksum.
sha256sum --check "$asset.sha256"          # Linux
# macOS: shasum -a 256 --check "$asset.sha256"

# Verify GitHub/Sigstore build provenance.
gh attestation verify "$asset" \
  --repo TheAxiomFoundation/axiom-rules-engine \
  --source-ref refs/tags/v0.1.1

# Only extract after both checks pass.
tar -xJf "$asset"
```

On macOS, `shasum --check` may also print
`WARNING: 1 line is improperly formatted` because the published `.sha256` file
has a trailing blank line
([issue #122](https://github.com/TheAxiomFoundation/axiom-rules-engine/issues/122)).
The check still reports the archive as `OK`.

## Use the v0.1.1 CLI

Release v0.1.1 provides `compile`, `run-compiled`, `emit-schemas`, and
`--version`. Its `compile` command accepts `--program` and `--output` only. It
resolves canonical cross-repo imports without a root flag.

```sh
axiom-rules-engine compile \
  --program rulespec-uk/uk-coventry/policies/coventry/council-tax-reduction.yaml \
  --output ctr.json
axiom-rules-engine run-compiled --artifact ctr.json < household.json
```

Do not pass `--rulespec-root` or use `compile-composed` with the v0.1.1 binary.
Those interfaces belong to the CLI built from current `main`.

> **Compile only from a canonical country monorepo checkout.** Because v0.1.1 resolves
> canonical imports by searching the filesystem rather than from explicitly declared roots, it
> can select an authority from a *neighbouring* checkout — silently compiling a different
> jurisdiction's text than the one your `--program` path points into
> ([issue #99](https://github.com/TheAxiomFoundation/axiom-rules-engine/issues/99)). Layouts
> known to resolve correctly are an exact `rulespec-<country>` monorepo and a full
> branch-suffixed copy of one. Layouts known to mis-resolve include standalone jurisdiction
> checkouts, sparse worktrees, nested `_axiom` directories, and reliance on cwd or environment
> discovery. Current `main` removes this search entirely in favour of the required
> `--rulespec-root`, which is why that flag exists.

## Use the CLI built from current main

Current `main` has a different CLI contract. Its `compile` and
`compile-composed` commands require the repeatable `--rulespec-root` option, with
one absolute path for each canonical country repository:

```sh
cargo run -- compile \
  --program /abs/path/to/rulespec-us/us/policies/example.yaml \
  --rulespec-root /abs/path/to/rulespec-us \
  --output compiled.json

cargo run -- compile-composed \
  --program /abs/path/to/program.yaml \
  --rulespec-root /abs/path/to/rulespec-us \
  --output compiled.json
```

## Artifact provenance

[Artifact releases](https://github.com/TheAxiomFoundation/axiom-rules-engine/releases)
carry Sigstore attestations and the corpus releases they cite are
Ed25519-signed against the org trust root — so a downloaded artifact's
provenance is verifiable independently of where you got the binary.
