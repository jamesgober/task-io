<h1 align="center">
    <img width="90px" height="auto" src="https://raw.githubusercontent.com/jamesgober/jamesgober/main/media/icons/hexagon-3.svg" alt="Triple Hexagon">
    <br><b>CHANGELOG</b>
</h1>
<p>
  All notable changes to <code>task-io</code> will be documented in this file. The format is based on <a href="https://keepachangelog.com/en/1.1.0/">Keep a Changelog</a>,
  and this project adheres to <a href="https://semver.org/spec/v2.0.0.html/">Semantic Versioning</a>.
</p>

---

## [Unreleased]

---

## [0.1.0] - 2026-10-13

The scaffold and the plan. No functional code yet.

### Added

- Crate manifest (edition 2024, MSRV 1.85, no dependencies), `Apache-2.0 OR MIT`
  license, README, REPS, CI (Linux, macOS, Windows on stable and 1.85; audit
  and deny), `deny.toml`, `clippy.toml`, `rustfmt.toml`, `.gitattributes`.
- `dev/PLAN.md`: purpose, consumers, layers, scope, the mandates (performance,
  stability, simplicity, testing), planned architecture (task memory, scheduler,
  topology, kinds of work, scopes, timers, actors, jobs, cluster, driver seam,
  simulation), planned API, failure model, security responsibilities,
  ecosystem, open decisions, and the decision log.
- `dev/BENCHMARKS.md`: the performance contract. Workloads, competitors,
  measurement protocol, reference hosts, and the 1.20 gate.
- `dev/RESEARCH.md`: the R&D program, the idea register, and the reading list.
- `dev/DIRECTIVES.md` and `dev/ROADMAP.md`.
- `VERSION` constant and a smoke test.

[Unreleased]: https://github.com/jamesgober/task-io/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jamesgober/task-io/releases/tag/v0.1.0
