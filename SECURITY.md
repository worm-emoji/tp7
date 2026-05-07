# Security Policy

`tp7` talks directly to a USB-connected TP-7 and can read, upload, rename, and delete files on the recorder. Treat write-path bugs and unsafe transfer behavior as security-sensitive when they could cause data loss.

## Reporting a Vulnerability

Use GitHub's private vulnerability reporting flow for this repository, or contact the repository owner privately using the contact details on their GitHub profile.

Please include:

- The affected command or workflow.
- The TP-7 firmware version and macOS version, if hardware is involved.
- Reproduction steps.
- Expected and actual behavior.
- Relevant logs or terminal output with personal file names redacted when needed.

## Data Safety

- Run `tp7 stat` or `tp7 ls -lah` before touching unknown remote files.
- Use `--dry-run` for destructive or write-path commands when validating behavior.
- Use small generated files for hardware tests.
- Keep independent backups of important recordings.

## Dependency Updates

The project depends on Rust crates for USB, MTP, MIDI, CLI parsing, and serialization. Dependency bumps should run the full smoke set and, when write behavior is affected, the hardware smoke script.
