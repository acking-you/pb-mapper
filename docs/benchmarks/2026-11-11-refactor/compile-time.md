# Build times before and after the refactor, 2026-11-11

How long the workspace takes to build before the refactor (3207797, the
0.11.0 release) and after it. Build time was not a goal of the refactor and
nothing was changed for its sake; this page records what happened to it.

Only measured figures are below. The raw rows are in
`compile-time-windows.csv` and `compile-time-linux.csv` (`label,scenario,
seconds,exit status`), and `compile_bench.sh` is the script that produced
them.

## Machine

| | |
|---|---|
| CPU | AMD Ryzen 9 9950X, 16 cores / 32 threads |
| RAM | 61.6 GiB; the WSL 2 guest is given 39 GiB |
| Windows | Windows 11 Enterprise 10.0.26300, `x86_64-pc-windows-msvc` |
| Linux | WSL 2, kernel 6.6.87.2, `x86_64-unknown-linux-gnu`, sources and build output on the guest's ext4 disk |
| Rust | rustc 1.99.1 (58cf8f9c3 2026-10-13), cargo 1.99.1 |

The two platforms' columns are not comparable with each other. On Windows
the workspace's profiles apply unchanged, so `dev` builds carry full debug
information. In the WSL guest the user's own cargo configuration sets
`[profile.dev] debug = "line-tables-only"`, which applied to both sides
there; its `sccache` wrapper was switched off for these runs
(`RUSTC_WRAPPER=`), so every run compiled for real.

## Method

Two checkouts side by side, the same distance from the drive's root: the
base at 3207797 and the refactored tree (Windows: 1ba36667; Linux:
8651b51c, which is 1ba36667 plus the last review's fixes, about 1,900 lines).
Three rounds; in each, both checkouts ran every scenario, and which went
first alternated between rounds. Crates were fetched beforehand and nothing
else was compiling. A scenario starts from an empty target directory, except
the four rebuilds, which reuse the directory `build` leaves and touch one
crate's `src/lib.rs` first.

| scenario | command |
|---|---|
| check | `cargo check --workspace --all-targets --all-features --locked` |
| build | `cargo build --workspace --all-features --locked` |
| rebuild after touching a crate | the `build` command again |
| test | `cargo test --workspace --all-features --no-run --locked` |
| sdk | `cargo build -p pb-mapper --locked` (what a user of the published SDK compiles) |
| release-cli | `cargo build --release --bin pb-mapper --locked` |
| release-all | `cargo build --release --workspace --all-features --locked` |

## Windows

Seconds of wall-clock time; the three runs in round order, then the medians.

| scenario | base 3207797 | refactored 1ba36667 | median | change |
|---|---|---|---|---|
| check | 50.9, 46.1, 44.3 | 53.2, 43.2, 43.5 | 46.1 → 43.5 | -5.6% |
| build | 88.5, 80.5, 80.7 | 88.2, 77.2, 77.0 | 80.7 → 77.2 | -4.3% |
| rebuild, pb-mapper-core touched | 15.4, 12.6, 13.3 | 14.4, 12.9, 13.2 | 13.3 → 13.2 | -0.8% |
| rebuild, pb-mapper-protocol touched | 13.8, 12.6, 12.7 | 13.6, 12.6, 12.3 | 12.7 → 12.6 | -0.8% |
| rebuild, pb-mapper-client touched | 13.1, 12.0, 12.0 | 12.5, 11.6, 11.8 | 12.0 → 11.8 | -1.7% |
| rebuild, pb-mapper-desktop touched | 10.5, 10.0, 9.7 | 10.0, 9.9, 9.4 | 10.0 → 9.9 | -1.0% |
| test | 141.3, 143.2, 127.0 | 128.1, 121.2, 112.9 | 141.3 → 121.2 | -14.2% |
| sdk | 29.8, 26.7, 27.1 | 30.1, 26.9, 27.6 | 27.1 → 27.6 | +1.8% |
| release-cli | 84.5, 75.3, 75.8 | 75.0, 68.0, 68.2 | 75.8 → 68.2 | -10.0% |
| release-all | 115.7, 102.2, 99.6 | 97.4, 89.3, 90.5 | 102.2 → 90.5 | -11.4% |

## Linux

| scenario | base 3207797 | refactored 8651b51c | median | change |
|---|---|---|---|---|
| check | 17.0, 16.1, 16.0 | 16.1, 16.4, 16.3 | 16.1 → 16.3 | +1.2% |
| build | 22.2, 22.0, 22.0 | 22.4, 21.7, 21.8 | 22.0 → 21.8 | -0.9% |
| rebuild, pb-mapper-core touched | 11.9, 11.9, 11.9 | 12.4, 11.8, 11.7 | 11.9 → 11.8 | -0.8% |
| rebuild, pb-mapper-protocol touched | 11.0, 11.0, 10.9 | 11.0, 10.9, 10.8 | 11.0 → 10.9 | -0.9% |
| rebuild, pb-mapper-client touched | 9.8, 9.7, 9.7 | 9.8, 10.0, 9.9 | 9.7 → 9.9 | +2.1% |
| rebuild, pb-mapper-desktop touched | 6.7, 6.7, 7.0 | 6.6, 6.5, 6.5 | 6.7 → 6.5 | -3.0% |
| test | 30.4, 31.1, 32.0 | 27.2, 27.0, 26.8 | 31.1 → 27.0 | -13.2% |
| sdk | 14.8, 14.9, 15.0 | 15.1, 14.7, 14.7 | 14.9 → 14.7 | -1.3% |
| release-cli | 30.9, 30.8, 30.5 | 28.6, 28.2, 28.1 | 30.8 → 28.2 | -8.4% |
| release-all | 39.6, 39.4, 39.3 | 36.4, 36.2, 36.0 | 39.4 → 36.2 | -8.1% |

## Reading the tables

- The test build and the two release builds are faster on both platforms: in
  every round the refactored tree was the faster of the two. On Linux, and
  for the release builds on Windows, every refactored run is also below every
  base run; the Windows test build's rounds overlap (its slowest refactored
  run, 128.1 s, against the base's fastest, 127.0 s).
- `check`, `build`, `sdk` and the rebuilds did not move. Their differences are
  inside the spread between rounds, which on Windows reaches a tenth of the
  figure (the base's `check`: 50.9, 46.1, 44.3) and on Linux is a few tenths
  of a second.
- On Windows a clean build waits for the third-party `windows` crate, which
  takes 33 to 36 s in a debug build with the features the workspace asks of
  it and sits ahead of pb-mapper-client, pb-mapper-desktop and the app's
  library. No refactoring of this workspace shortens that.
- Which changes account for the faster test and release builds was not
  measured. Fewer targets are linked: the CLI's integration tests are nine
  binaries where there were eleven, and seven unreferenced examples are gone.
