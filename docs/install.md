# Install a released binary

Download archive + `.sha256` via:

```sh
gh release download v0.1.0 --repo TheAxiomFoundation/axiom-rules-engine --pattern <asset> --pattern <asset>.sha256
```

```sh
sha256sum --check <asset>.sha256
```

macOS:

```sh
shasum -a 256 --check <asset>.sha256
```

```sh
gh attestation verify <asset> --repo TheAxiomFoundation/axiom-rules-engine --source-ref refs/tags/v0.1.0
```

Only extract after both checks pass.

## Trusted rule content

The binary alone executes compiled artifacts / JSON requests; compiling canonical `us:` imports additionally requires a rulespec checkout or downloaded program-artifacts release, pointed to via `AXIOM_RULESPEC_REPO_ROOTS`. [Artifact releases](https://github.com/TheAxiomFoundation/axiom-rules-engine/releases) carry sigstore attestations and [corpus releases](jurisdiction-repos.md) are Ed25519-signed.
