# Fossick

Instant, Everything-style file-name search for Linux: indexes every file and folder on every
mounted filesystem, finds them as you type, and stays current in real time.

> **Status: early development (Phase 1 of 6).** Nothing is packaged yet. See
> [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the design and plan and
> [`docs/TODO.md`](docs/TODO.md) for what is missing.

Fossick is an independent project. It isn’t affiliated with voidtools; it reimplements the
behaviour and search syntax of voidtools Everything for Linux.

## Try it (developers)

```sh
cargo run --release -p fossick -- --local /usr 'ext:so size:>1mb'
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
