# Contributing to Rinflow for Windows

Thanks for your interest. Clone [rinflow-core](https://github.com/rinflow-dev/rinflow-core) next to this repository, because the agent builds it from source, and see the [prerequisites](README.md#prerequisites).

## Building and testing

```powershell
.\dev.ps1 run                        # stop what is running, build, start it
.\build.cmd -Configuration Release   # build only
.\installer\build-installer.ps1      # build the setup program
```

Before opening a pull request, run these in both `agent` and `input`:

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

UI text lives in `app/Strings`, in English and Chinese; add both. Keep `build.ps1` and `dev.ps1` ASCII-only, because Windows PowerShell 5.1 reads scripts without a BOM in the system code page.

If `agent/Cargo.lock` or `input/Cargo.lock` changes, regenerate `THIRD_PARTY_NOTICES.txt` as shown in [its script](https://github.com/rinflow-dev/rinflow-core/blob/main/scripts/third-party-notices.ps1).

## Commits and pull requests

- Keep pull requests focused, and open an issue first for larger changes.
- Use [Conventional Commits](https://www.conventionalcommits.org/) for messages: `feat: ...`, `fix: ...`, `docs: ...`, `refactor: ...`, `test: ...`, `build: ...`, `ci: ...`, `chore: ...`.
- Add user-visible changes to `CHANGELOG.md`.

## Protocol and ABI changes

The clients link `rinflow-core` from source. A change to the wire protocol or the C ABI affects every client, so bump the versions as described in [the core README](https://github.com/rinflow-dev/rinflow-core#changing-the-protocol-or-abi) and update the clients in step.

## License

Rinflow is licensed under the [Mozilla Public License 2.0](LICENSE). Unless you state otherwise, your contribution is licensed the same way. New source files start with this notice, in the file's comment syntax:

```
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.
```

Please do not copy code from projects with incompatible licenses. Other input-sharing projects may be consulted for how they behave, but their code may not be reused unless its license allows it.

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
