# Nixblend

A package manager for NixOS with the feel of APT or Pacman that respects the
declarative nature of Nix: packages are declarations in a file you own, and every
change is validated, shown as a diff and confirmed before it is written.

```bash
cargo build --release
./target/release/nixblend                      # interactive interface
./target/release/nixblend search ripgrep       # or a single command
sudo ./target/release/nixblend install ripgrep
```

The first `install` or `remove` asks for the file to manage and saves it in
`$XDG_DATA_HOME/nixblend/config.toml` (by default
`~/.local/share/nixblend/config.toml`), if the file proves usable. To choose another
file, edit or delete that one.

## Interactive interface

Run `nixblend` without a command, in a terminal. It lists what the managed file declares,
searches nixpkgs, and runs the same operations as the commands, with the same validations
and protections. Run it with `sudo` if the managed file needs it, as with the commands.

| Key | Action |
| --- | --- |
| `/` | Search packages. While typing, every letter is text |
| Up, Down, PgUp, PgDn | Move in the list |
| Enter | Details of the selected package, in the panel or on their own in narrow terminals |
| `i` | Install the selected search result |
| `r` | Remove the selected declaration |
| `R` | Reinstall: validate the configuration and rebuild |
| Esc | Back, or stop what is running |
| `y`, `n` | Answer a question. Nothing is changed without a `y` |
| `?` | Show the keys |
| `q` | Quit |
| Ctrl+C | Quit from anywhere, ending what is running |

An operation shows the package, its source, the file, the diff and the question on one
screen. The rebuild takes over the terminal while it runs, so that its output and the `sudo`
password prompt work as usual, and the interface comes back when it ends.

It adapts to the terminal: real colors where available, then 256 or 16 colors, none with
`NO_COLOR`, and plain ASCII in a locale that is not UTF-8. The details panel needs 90 columns
and the logo 20 rows. Without a terminal, as in a pipe, it explains what to use instead.

The Nix logo is the official artwork from
[NixOS/nixos-artwork](https://github.com/NixOS/nixos-artwork), reduced from the
`nixos-icons` package.

## Commands

| Command | What it does |
| --- | --- |
| `install <package> [--no-rebuild]` | Validates a package and declares it |
| `remove <package> [--no-rebuild]` | Removes exactly one declaration |
| `search <term>` | Searches names and descriptions in nixpkgs |
| `list [--active]` | Lists the declarations, optionally against the active generation |
| `info <package>` | Shows the metadata of a package and whether it is declared |
| `reinstall <package>` | Validates the configuration and rebuilds it |

### install

1. Checks that the file can be edited, is valid Nix and does not declare the package.
2. Confirms that the package exists and reads its metadata, using the nixpkgs revision
   in `/etc/nixos/flake.lock` and the system configuration, so overlays and settings
   like `allowUnfree` count. A package the configuration refuses needs a decision.
3. If the same name is declared from another source, explains that and asks. It never
   adds a second declaration silently.
4. Validates the result with the real Nix parser, shows the diff and asks.
5. Saves a backup and replaces the file atomically, keeping its owner and mode.
6. Offers the rebuild. If it fails, the edit is kept unless you choose to undo it.

A typo is answered from the list of attribute names, which takes about a second, with the
option of searching everything. Only that sees nested attributes, so `kate` finds
`kdePackages.kate` that way. A full search only runs if you agree, since the first one for
a nixpkgs revision takes about a minute and several GB of memory.

### search

Searches names and descriptions, showing details a page at a time. A term that is not part
of any name but is one edit away from one, like `firefx`, asks first; only an explicit `n`
skips the search.

### remove

Finds the declaration, shows it with its line, and removes only it. Comments on other
lines and the rest of the file stay byte for byte; a comment on the same line goes with
the entry. If a name matches several declarations, nothing changes until you name one:

```bash
sudo nixblend remove firefox           # stops if both of the next two are declared
sudo nixblend remove pkgs.firefox
sudo nixblend remove unstable.firefox
```

Two identical entries cannot be told apart by name, so they must be fixed by hand.

### list

Reflects the file, not imperative installations, and needs no Nix tools. With
`--active`, it compares with `/run/current-system`:

- For nixpkgs packages, by the exact store path name, so `no` means that version is not
  in the generation.
- For other sources, by the attribute name. That can only confirm presence, so anything
  else shows `unknown`.

### reinstall

The Nix store is immutable and the system is declarative, so nothing is removed or added
back and the file is not touched. It validates the configuration and rebuilds it, which
**does not necessarily download or compile the package again**.

## Package names

| Name | Meaning |
| --- | --- |
| `git`, `pkgs.git` | Attribute of the system's nixpkgs |
| `kdePackages.kate` | Nested attribute |
| `unstable.vscode` | Attribute of a package set the file imports, as in `unstable = import inputs.nixpkgs-unstable { ... };` |

A package declared directly from a flake input, like
`inputs.foo.packages.${system}.default`, is known by the input's name, `foo`.

## Supported file formats

The list must be `[ ... ]` or `with pkgs; [ ... ]`, with `pkgs` bound by the file, and be
one of:

- A top-level module attribute: `environment.systemPackages`, `home.packages` or
  `users.users.<name>.packages`, optionally under `config`, written with dots or nested
  sets such as `home = { packages = ...; };`.
- The whole expression of the file.

Function headers (including `@` patterns), `let ... in` and `with ...;` may come first.
Anything else, such as `++`, `lib.mkIf` or more than one list, is refused without
changing the file. This is not a parser for the whole language: it recognizes these
structures, and the real Nix parser validates every result.

## Safety

- **Least privilege.** Under `sudo`, the configuration is handled as the original user.
  The managed file is too, whenever that user can do it; otherwise root privileges apply
  only to files under `/etc/nixos`. Searches and syntax checks never run as root.
- **Tools.** While elevated, tools come from `/run/current-system/sw/bin`, must be owned
  by root and not writable by others, and inherit only a short list of harmless variables,
  with `HOME` set to `/root`.
- **Limits of `sudo`.** Allowing someone to run only `nixblend` through `sudo` is not a
  security boundary: it can rebuild the system, and if `/etc/nixos` belongs to that person,
  the rebuild runs as root a configuration they wrote.
- **Paths.** The stored path is validated on every run, and the configuration must not be
  writable by group or others, nor sit in a directory that is. The directory of the managed
  file is opened once and the file is reached through it without following symlinks, so
  swapping a path after the checks has no effect. With root privileges, a file with other
  hard links is refused.
- **Transactions.** Changes to the content, owner or mode made while a command waits are
  detected, not overwritten. The previous content is kept in `.<file>.nixblend-backup`, next to the file
  and with its owner and mode.
- **Interruption.** Ctrl+C and `SIGTERM` end the running tool and remove temporary files. A
  rebuild is left to finish, since stopping it halfway is worse, unless Ctrl+C reaches it
  from the terminal. After `SIGKILL` or a power loss, the next run removes temporary files.
- **Time limits.** Queries give up after 3 minutes and full searches after 10, reporting
  the real error. A failed search is never reported as a missing package. The rebuild has
  no limit, because it can legitimately take hours, and its output is shown as it runs.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Done, cancelled or nothing to do |
| 1 | Nothing was installed or removed: invalid input, unknown package, failed validation or failed edit |
| 2 | Invalid command line |
| 3 | The rebuild failed. Any edit stays applied, unless you chose to undo it |
| 141 | Whoever was reading the output went away, as in `nixblend search foo \| head`. An edit may have been applied |

## Architecture

Hexagonal, as modules of a single crate. Dependencies point inwards:

```text
cli, tui ──> application ──> domain
                   ^
                   │ implements the ports
            infrastructure
```

- `domain`: packages, sources, declarations and the rules that relate them. It knows
  nothing about the terminal, files or Nix tools.
- `application`: one module per use case, and `ports.rs` with the interfaces they need.
- `infrastructure`: Nix tools, the parser for package files, atomic writes, processes,
  signals, configuration and privileges.
- `cli`: arguments and presentation for single commands.
- `tui`: the interactive interface. The use cases run in their own thread and talk to it
  through the `Interaction` port, so it holds no business rules. `app.rs` is the state and
  `view.rs` the drawing, both testable without a terminal.
- `main.rs`: the only place that connects all of them.

To use another package source, implement `PackageCatalog` and `CatalogProvider`.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Do not run the tests as root: as root the binary ignores `PATH`, so the simulated tools
would be replaced by the real ones. The tests refuse to start in that case.

`tests/tui.rs` drives the interface in a real pseudo-terminal, through `script`.

`tests/sudo.rs` exercises the elevated paths inside a user namespace. It needs subordinate
IDs and a NixOS system profile, and reports `SKIPPED` without them. Set
`NIXBLEND_REQUIRE_SUDO_TESTS=1` to make that a failure.
