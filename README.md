# Red Dead Redemption Fix
![GitHub Downloads (all assets, all releases)](https://img.shields.io/github/downloads/PolarWizard/red-dead-redemption-fix/total)

***This project is designed exclusively for Windows due to its reliance on Windows-specific APIs.***

## Fixes
- Constrains the HUD, minimap, subtitles and menus to a centered 16:9 region, with the mouse following the menus
- Skips the logo video at startup

## Build and Install
Requires:
- [Rust](https://www.rust-lang.org/tools/install) (cargo)
- [Python 3](https://www.python.org/downloads/)

1. Clone:
```ps1
git clone https://github.com/PolarWizard/red-dead-redemption-fix.git
cd red-dead-redemption-fix
```
2. Install [Ultimate ASI Loader](https://github.com/ThirteenAG/Ultimate-ASI-Loader) into the game folder (only needed once):
```ps1
python scripts/loader.py "<path to 'Red Dead Redemption' folder>"
```
3. Build and install the fix:
```ps1
python scripts/install.py "<path to 'Red Dead Redemption' folder>"
```
This builds a release DLL with `cargo` and copies it to `<game folder>/scripts/red_dead_redemption_fix.asi`, along with a default config on first install. Pass `--debug` to build and install a debug build instead.

If you're using VS Code, the same steps are wired up as tasks in `.vscode/tasks.json` (`loader`, `install`, `build and install`) -- edit the game folder path in there to match your install first.

## Configuration
- Edit `<game folder>/scripts/red_dead_redemption_fix.toml`

## License
Distributed under the MIT License. See [LICENSE](LICENSE) for more information. Third-party license text for every dependency can be generated with `python scripts/licenses.py`, which writes it out to `LICENSES`.
