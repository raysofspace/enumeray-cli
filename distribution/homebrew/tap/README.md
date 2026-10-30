# Enumeray Homebrew tap

This tap distributes the qualified native macOS ARM64 and Intel archives from
[Enumeray releases](https://github.com/raysofspace/enumeray-cli/releases).
macOS 15 or newer is required.

The first release is **0.1.0-alpha.1**. Commands and APIs may change, and alpha
updates may require fresh installations. No in-place alpha upgrade compatibility
is promised. Preserve an existing installation's state and matching binaries.

After its qualified formula has been reviewed and merged:

```sh
brew install raysofspace/tap/enumeray
enumeray --version
```

The formula installs no service. Review the packaged installation guide before
coordinating connector and compute-runner replacements. Removing the package
preserves separately managed configuration, credentials and connector journals.

Release automation opens an immutable version-specific formula PR. A maintainer
reviews and merges it after both native macOS checks pass. Tap CI verifies the
signed published manifest and exact generated formula, installs and tests the
package, then removes it while preserving its initialized journal. An empty
initial tap does not establish installation qualification.
