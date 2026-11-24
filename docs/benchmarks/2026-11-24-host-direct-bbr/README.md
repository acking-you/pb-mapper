# The host's direct video: Cubic against BBR

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1. Other worktrees were building on the same machine (a load
average of about 4.4). Everything runs in one process on loopback, through
the testkit's `lossy_udp` hop: each way it holds every datagram half the
round trip in one ordered queue, drops a share at random from a fixed seed,
and, where a policer is named, drops what a token bucket of 50 ms at its
rate cannot pay for. Loss is each way. The sockets ask for 4 MiB buffers
(macOS grants up to `kern.ipc.maxsockbuf`, 8 MiB here). None of it is a real
WAN path; see the last section.

The question is the plan's gate for sending the host's direct QUIC under
BBR (`quinn::congestion::BbrConfig::default()`, this quinn fork's) instead
of Cubic: ship only if the IDR p95 falls and the delivered rate rises at
0.1–1% loss, and if, behind a policer with the receipts setting the rate,
neither the p99 frame latency nor the loss rises.

## Open loop: `direct_cc_bench`

```
cargo run --release -p pb-mapper-desktop --example direct_cc_bench -- \
  --seconds 60 --jobs 4
```

Built at `da1ac06`. Two quinn endpoints with pb-mapper's direct transport
(`direct_transport_config`), the sender's under the cell's controller and
the receiver's under Cubic. The sender captures every 1/60 s: a P-frame of
the video rate (8 333 bytes at 4 Mbit/s, 16 667 at 8) and a 150 KB IDR
every 2 s, admitted under the host's credit window (2 × the rate × (the
round trip + 100 ms)) and released by a receipt per frame; a frame the
window holds stalls the capture, whose ticks are skipped (`skipped`).
There is no rate control above the window. Each cell ran 60 s, four cells
at once. Latencies are from the capture to the frame's last byte read, in
ms; IDR figures leave out the session's first IDR (`1st IDR`); `Mbit/s` is
what arrived while the cell ran; `lost pkts` and `cong. ev.` are the
sender's quinn path stats. Every line is in [open-loop.txt](open-loop.txt).

| RTT ms | loss % | policer Mbit/s | video Mbit/s | cc | P p50 | P p95 | P p99 | IDR p50 | IDR p95 | IDR p99 | 1st IDR | Mbit/s | skipped | lost pkts | cong. ev. |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 20 | 0 | none | 4 | cubic | 13.7 | 15.5 | 21.8 | 27.6 | 44.6 | 56.1 | 132.0 | 4.53 | 36 | 0 | 0 |
| 20 | 0 | none | 4 | bbr | 13.6 | 15.5 | 20.0 | 15.2 | 55.5 | 81.2 | 152.9 | 4.55 | 19 | 0 | 0 |
| 20 | 0 | none | 8 | cubic | 13.8 | 15.6 | 18.3 | 20.6 | 40.3 | 46.6 | 133.4 | 8.53 | 3 | 0 | 0 |
| 20 | 0 | none | 8 | bbr | 13.8 | 15.6 | 19.8 | 15.4 | 46.4 | 137.2 | 281.0 | 8.50 | 13 | 0 | 0 |
| 20 | 0 | 5 | 4 | cubic | 13.1 | 14.7 | 32.4 | 223.2 | 268.8 | 282.2 | 301.0 | 4.14 | 387 | 3766 | 837 |
| 20 | 0 | 5 | 4 | bbr | 13.0 | 14.6 | 23.6 | 265.4 | 329.0 | 331.0 | 238.2 | 4.07 | 448 | 11293 | 451 |
| 20 | 0 | 5 | 8 | cubic | 226.0 | 389.9 | 444.3 | 438.4 | 462.9 | 475.4 | 250.1 | 4.88 | 1636 | 1058 | 703 |
| 20 | 0 | 5 | 8 | bbr | 208.5 | 385.2 | 475.6 | 455.7 | 554.5 | 557.5 | 321.0 | 4.81 | 1668 | 90922 | 14107 |
| 20 | 0.1 | none | 4 | cubic | 12.6 | 14.3 | 40.1 | 43.2 | 64.6 | 80.4 | 97.5 | 4.50 | 61 | 30 | 30 |
| 20 | 0.1 | none | 4 | bbr | 12.9 | 14.3 | 34.1 | 14.7 | 38.4 | 39.9 | 36.3 | 4.55 | 11 | 31 | 31 |
| 20 | 0.1 | none | 8 | cubic | 13.7 | 24.1 | 58.2 | 47.4 | 106.8 | 129.3 | 93.1 | 8.53 | 3 | 66 | 66 |
| 20 | 0.1 | none | 8 | bbr | 13.4 | 14.9 | 36.4 | 14.4 | 37.7 | 38.8 | 42.5 | 8.53 | 0 | 66 | 66 |
| 20 | 0.1 | 5 | 4 | cubic | 12.6 | 18.2 | 51.7 | 227.1 | 279.9 | 281.9 | 229.7 | 4.13 | 390 | 3390 | 790 |
| 20 | 0.1 | 5 | 4 | bbr | 12.8 | 14.5 | 53.2 | 261.6 | 340.1 | 340.8 | 278.4 | 4.06 | 459 | 9186 | 470 |
| 20 | 0.1 | 5 | 8 | cubic | 225.9 | 387.5 | 447.9 | 435.7 | 468.5 | 521.4 | 248.6 | 4.86 | 1646 | 1027 | 718 |
| 20 | 0.1 | 5 | 8 | bbr | 212.3 | 363.6 | 443.2 | 441.1 | 534.2 | 547.0 | 285.1 | 4.87 | 1642 | 58407 | 15686 |
| 20 | 1 | none | 4 | cubic | 17.1 | 84.2 | 146.2 | 193.3 | 378.8 | 382.9 | 102.3 | 4.19 | 336 | 228 | 224 |
| 20 | 1 | none | 4 | bbr | 13.1 | 36.0 | 44.4 | 35.2 | 37.7 | 39.7 | 46.1 | 4.54 | 23 | 241 | 239 |
| 20 | 1 | none | 8 | cubic | 165.6 | 322.3 | 432.0 | 343.0 | 494.8 | 660.7 | 103.7 | 5.96 | 1150 | 316 | 312 |
| 20 | 1 | none | 8 | bbr | 13.5 | 36.8 | 50.9 | 36.0 | 38.3 | 38.7 | 88.5 | 8.53 | 0 | 456 | 442 |
| 20 | 1 | 5 | 4 | cubic | 18.7 | 110.0 | 231.9 | 243.9 | 301.4 | 371.8 | 236.4 | 4.06 | 457 | 904 | 404 |
| 20 | 1 | 5 | 4 | bbr | 13.0 | 37.4 | 223.6 | 358.9 | 433.0 | 507.3 | 216.8 | 3.92 | 581 | 7823 | 475 |
| 20 | 1 | 5 | 8 | cubic | 235.2 | 428.4 | 515.3 | 488.8 | 600.8 | 642.1 | 234.3 | 4.49 | 1813 | 728 | 526 |
| 20 | 1 | 5 | 8 | bbr | 210.3 | 376.3 | 445.9 | 442.5 | 493.3 | 512.8 | 232.4 | 4.86 | 1647 | 61783 | 15405 |
| 60 | 0 | none | 4 | cubic | 32.9 | 34.3 | 64.0 | 62.1 | 84.8 | 103.1 | 289.0 | 4.44 | 112 | 0 | 0 |
| 60 | 0 | none | 4 | bbr | 32.7 | 34.2 | 48.7 | 34.7 | 37.1 | 42.5 | 182.5 | 4.51 | 49 | 0 | 0 |
| 60 | 0 | none | 8 | cubic | 32.7 | 35.0 | 37.9 | 49.3 | 65.2 | 74.7 | 286.1 | 8.51 | 9 | 0 | 0 |
| 60 | 0 | none | 8 | bbr | 33.0 | 34.5 | 36.8 | 37.7 | 51.7 | 63.4 | 196.2 | 8.52 | 7 | 0 | 0 |
| 60 | 0 | 5 | 4 | cubic | 32.6 | 34.5 | 273.1 | 266.4 | 284.0 | 284.2 | 352.5 | 4.05 | 465 | 1903 | 566 |
| 60 | 0 | 5 | 4 | bbr | 32.6 | 34.5 | 281.3 | 268.3 | 311.0 | 444.6 | 282.6 | 4.01 | 498 | 4495 | 370 |
| 60 | 0 | 5 | 8 | cubic | 280.2 | 525.3 | 586.3 | 503.3 | 585.6 | 600.1 | 368.9 | 4.65 | 1737 | 356 | 222 |
| 60 | 0 | 5 | 8 | bbr | 245.1 | 479.1 | 567.1 | 546.2 | 618.2 | 625.9 | 404.7 | 4.77 | 1683 | 27091 | 7778 |
| 60 | 0.1 | none | 4 | cubic | 33.3 | 94.0 | 216.7 | 149.6 | 443.2 | 488.6 | 289.7 | 4.21 | 319 | 30 | 28 |
| 60 | 0.1 | none | 4 | bbr | 33.0 | 37.3 | 93.7 | 34.8 | 97.1 | 97.5 | 198.3 | 4.49 | 64 | 30 | 30 |
| 60 | 0.1 | none | 8 | cubic | 191.1 | 491.8 | 818.8 | 420.4 | 940.9 | 951.9 | 284.3 | 5.68 | 1275 | 46 | 46 |
| 60 | 0.1 | none | 8 | bbr | 33.1 | 62.7 | 96.8 | 34.6 | 93.7 | 98.0 | 112.3 | 8.53 | 0 | 66 | 66 |
| 60 | 0.1 | 5 | 4 | cubic | 40.3 | 176.4 | 296.4 | 278.9 | 498.6 | 648.2 | 357.1 | 3.89 | 604 | 525 | 198 |
| 60 | 0.1 | 5 | 4 | bbr | 33.0 | 45.3 | 314.2 | 305.1 | 452.7 | 648.1 | 263.0 | 3.91 | 589 | 4825 | 319 |
| 60 | 0.1 | 5 | 8 | cubic | 302.3 | 614.4 | 860.2 | 536.5 | 844.0 | 878.8 | 368.7 | 4.17 | 1955 | 285 | 168 |
| 60 | 0.1 | 5 | 8 | bbr | 241.9 | 493.6 | 556.7 | 517.5 | 611.3 | 617.8 | 389.8 | 4.77 | 1683 | 32186 | 8633 |
| 60 | 1 | none | 4 | cubic | 227.7 | 584.4 | 949.8 | 850.3 | 1239.9 | 1411.0 | 322.3 | 2.11 | 2204 | 113 | 112 |
| 60 | 1 | none | 4 | bbr | 33.3 | 96.5 | 113.2 | 96.2 | 158.9 | 253.3 | 195.2 | 4.40 | 152 | 235 | 225 |
| 60 | 1 | none | 8 | cubic | 746.4 | 1651.1 | 1938.6 | 1259.4 | 1726.4 | 1868.3 | 322.3 | 2.14 | 2867 | 114 | 114 |
| 60 | 1 | none | 8 | bbr | 33.9 | 97.1 | 112.0 | 95.9 | 98.3 | 106.2 | 171.0 | 8.50 | 11 | 457 | 446 |
| 60 | 1 | 5 | 4 | cubic | 230.3 | 613.2 | 906.8 | 864.7 | 1312.4 | 1511.4 | 324.7 | 2.10 | 2215 | 113 | 112 |
| 60 | 1 | 5 | 4 | bbr | 33.3 | 104.4 | 291.5 | 293.9 | 386.1 | 423.7 | 339.1 | 3.96 | 544 | 4392 | 483 |
| 60 | 1 | 5 | 8 | cubic | 741.1 | 1673.1 | 1955.7 | 1236.3 | 1905.6 | 2019.7 | 318.1 | 2.11 | 2872 | 123 | 120 |
| 60 | 1 | 5 | 8 | bbr | 247.7 | 549.8 | 703.8 | 617.5 | 805.9 | 835.7 | 461.4 | 4.58 | 1771 | 46565 | 5719 |
| 120 | 0 | none | 4 | cubic | 62.6 | 64.3 | 84.4 | 96.5 | 130.2 | 164.0 | 568.8 | 4.52 | 39 | 0 | 0 |
| 120 | 0 | none | 4 | bbr | 63.3 | 64.7 | 65.5 | 65.6 | 70.9 | 86.8 | 195.5 | 4.56 | 6 | 0 | 0 |
| 120 | 0 | none | 8 | cubic | 64.0 | 67.4 | 74.3 | 88.2 | 107.5 | 115.0 | 568.7 | 8.47 | 24 | 0 | 0 |
| 120 | 0 | none | 8 | bbr | 63.8 | 65.2 | 66.4 | 64.8 | 66.6 | 74.2 | 195.0 | 8.52 | 1 | 0 | 0 |
| 120 | 0 | 5 | 4 | cubic | 69.0 | 297.9 | 371.7 | 315.6 | 398.6 | 404.3 | 572.3 | 4.07 | 440 | 126 | 81 |
| 120 | 0 | 5 | 4 | bbr | 63.0 | 570.8 | 686.3 | 604.6 | 739.0 | 857.9 | 506.5 | 3.47 | 985 | 6257 | 382 |
| 120 | 0 | 5 | 8 | cubic | 388.7 | 665.8 | 734.5 | 612.6 | 716.8 | 738.3 | 567.3 | 4.45 | 1827 | 223 | 110 |
| 120 | 0 | 5 | 8 | bbr | 338.1 | 720.1 | 927.8 | 728.3 | 989.6 | 1184.0 | 619.6 | 4.28 | 1901 | 20382 | 4420 |
| 120 | 0.1 | none | 4 | cubic | 137.0 | 524.4 | 817.5 | 528.6 | 1148.5 | 1458.1 | 569.9 | 3.28 | 1156 | 22 | 22 |
| 120 | 0.1 | none | 4 | bbr | 63.2 | 102.6 | 185.0 | 65.5 | 187.7 | 188.9 | 209.8 | 4.50 | 55 | 30 | 30 |
| 120 | 0.1 | none | 8 | cubic | 458.8 | 1352.3 | 1988.4 | 880.0 | 1824.3 | 1989.3 | 566.1 | 3.64 | 2188 | 28 | 28 |
| 120 | 0.1 | none | 8 | bbr | 63.3 | 151.9 | 185.8 | 65.2 | 187.6 | 189.0 | 138.4 | 8.48 | 19 | 66 | 66 |
| 120 | 0.1 | 5 | 4 | cubic | 178.3 | 558.4 | 839.9 | 558.1 | 1254.8 | 1461.2 | 574.2 | 3.22 | 1206 | 28 | 26 |
| 120 | 0.1 | 5 | 4 | bbr | 62.9 | 643.3 | 957.7 | 710.0 | 1009.8 | 1010.4 | 391.2 | 3.28 | 1158 | 6664 | 371 |
| 120 | 0.1 | 5 | 8 | cubic | 583.9 | 1313.1 | 1869.0 | 878.6 | 1637.6 | 1908.1 | 568.0 | 3.17 | 2402 | 189 | 83 |
| 120 | 0.1 | 5 | 8 | bbr | 345.3 | 647.1 | 743.5 | 644.3 | 765.5 | 809.6 | 500.7 | 4.40 | 1850 | 17358 | 4467 |
| 120 | 1 | none | 4 | cubic | 1199.1 | 2280.9 | 3059.7 | 1801.6 | 2706.0 | 2964.1 | 630.2 | 1.10 | 3107 | 60 | 60 |
| 120 | 1 | none | 4 | bbr | 63.8 | 186.5 | 233.5 | 186.4 | 281.5 | 297.9 | 326.8 | 4.26 | 274 | 231 | 228 |
| 120 | 1 | none | 8 | cubic | 2690.7 | 4347.6 | 5115.2 | 3533.7 | 4647.0 | 5221.4 | 630.4 | 1.10 | 3325 | 61 | 61 |
| 120 | 1 | none | 8 | bbr | 103.1 | 196.7 | 235.7 | 186.2 | 190.9 | 318.6 | 315.6 | 8.12 | 181 | 434 | 424 |
| 120 | 1 | 5 | 4 | cubic | 1185.6 | 2246.4 | 2949.4 | 1800.0 | 2596.7 | 3467.9 | 634.1 | 1.10 | 3107 | 60 | 60 |
| 120 | 1 | 5 | 4 | bbr | 64.4 | 563.7 | 931.0 | 596.1 | 1129.3 | 1396.3 | 377.3 | 3.29 | 1137 | 6024 | 509 |
| 120 | 1 | 5 | 8 | cubic | 2715.4 | 4388.8 | 5092.2 | 3620.7 | 5224.4 | 5369.8 | 635.8 | 1.09 | 3328 | 61 | 61 |
| 120 | 1 | 5 | 8 | bbr | 337.2 | 900.7 | 1050.3 | 951.0 | 1225.1 | 1251.8 | 761.0 | 4.01 | 2023 | 22408 | 3253 |

At 0.1% and 1% loss without a policer, BBR's IDR p95 was the lower in all
12 pairs, and its delivered rate the higher in 11: equal only at 20 ms,
0.1% and 8 Mbit/s, where Cubic delivered everything too. Cubic fell
furthest at 120 ms and 1%: 1.10 Mbit/s of 4 and of 8, with P-frames at a
p50 of 1.2 s and 2.7 s, against BBR's 4.26 and 8.12 Mbit/s at 64 and
103 ms. Behind a 5 Mbit/s policer BBR lost 2.4 to 379 times as many
packets as Cubic in all 18 pairs (6 257 against 126 at 120 ms, no random
loss, 4 Mbit/s; where there was random loss too, Cubic also delivered
less), and its P-frame p99 was the higher in 7 of the 18, and in 4 of the
6 without random loss.

## Closed loop: `slow_link::direct_quic_on_lossy_links`

```
cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture \
  --exact slow_link::direct_quic_on_lossy_links
```

Built at `a32cd5b`, run three times. The real session, with the synthetic
encoder at 1080p and 60 FPS, Balanced, and the viewer's receipts setting
the rate (`arrival_time` on), over one QUIC connection with the direct
transport through the hop: the host's end dials the host's listener for
the stream the viewer opens, as a direct provider does, and sends under the
run's controller. After 6 s on the link, the viewer asks for an IDR every
2 s for 20 s. Latencies are from the capture to the unit received whole;
the IDR wait is from the request to its IDR; `sent Mbit/s` is the host's
mean `sent_bitrate`; `lost %` is quinn's lost packets over those sent
while measured. Each figure is run 1 / run 2 / run 3; every line is in
[closed-loop-1.txt](closed-loop-1.txt), [closed-loop-2.txt](closed-loop-2.txt)
and [closed-loop-3.txt](closed-loop-3.txt).

| link | cc | P p50 ms | P p99 ms | IDR p95 ms | IDR wait p50 ms | IDR wait p95 ms | fps | sent Mbit/s | lost % |
|---|---|---|---|---|---|---|---|---|---|
| rtt60_loss0.1 | cubic | 48.1 / 48.2 / 50.1 | 224.7 / 202.7 / 211.9 | 219.4 / 227.9 / 259.2 | 130.4 / 116.9 / 119.8 | 262.4 / 276.1 / 294.7 | 59.2 / 59.5 / 59.0 | 4.48 / 4.75 / 4.45 | 0.17 / 0.17 / 0.17 |
| rtt60_loss0.1 | bbr | 40.8 / 40.7 / 40.7 | 105.9 / 106.2 / 105.4 | 106.5 / 107.0 / 105.7 | 85.1 / 81.3 / 85.7 | 142.0 / 148.9 / 147.2 | 60.6 / 60.1 / 60.3 | 8.20 / 8.20 / 8.20 | 0.14 / 0.15 / 0.15 |
| rtt60_loss1 | cubic | 46.7 / 47.8 / 50.2 | 201.1 / 200.2 / 234.8 | 115.9 / 234.8 / 222.0 | 114.4 / 109.9 / 157.2 | 156.4 / 271.5 / 264.1 | 56.6 / 58.5 / 55.6 | 1.23 / 1.26 / 1.42 | 1.08 / 1.09 / 1.14 |
| rtt60_loss1 | bbr | 41.1 / 41.0 / 41.2 | 137.0 / 117.7 / 122.8 | 170.6 / 106.1 / 117.3 | 86.4 / 86.0 / 89.6 | 213.2 / 154.2 / 165.3 | 58.8 / 59.4 / 58.7 | 5.62 / 5.26 / 6.34 | 0.98 / 1.00 / 1.00 |
| rtt120_loss0.1 | cubic | 86.2 / 103.2 / 95.5 | 350.6 / 280.2 / 315.0 | 272.1 / 271.2 / 291.1 | 179.5 / 191.9 / 176.4 | 356.4 / 380.2 / 368.8 | 54.4 / 55.4 / 54.5 | 3.08 / 3.22 / 3.20 | 0.18 / 0.18 / 0.18 |
| rtt120_loss0.1 | bbr | 71.0 / 71.0 / 71.0 | 195.8 / 303.7 / 195.7 | 74.2 / 469.2 / 73.1 | 144.1 / 146.4 / 140.9 | 196.1 / 531.8 / 149.2 | 59.1 / 58.9 / 59.1 | 8.14 / 7.69 / 8.14 | 0.14 / 0.13 / 0.14 |
| rtt120_loss1 | cubic | 82.5 / 83.1 / 82.7 | 413.4 / 432.4 / 324.8 | 750.5 / 230.5 / 195.6 | 201.2 / 204.7 / 208.3 | 819.3 / 439.6 / 265.5 | 41.4 / 37.9 / 38.6 | 0.84 / 0.74 / 0.69 | 0.89 / 0.79 / 0.84 |
| rtt120_loss1 | bbr | 78.4 / 79.3 / 89.2 | 211.8 / 212.7 / 306.6 | 213.2 / 211.4 / 211.2 | 261.3 / 158.2 / 191.2 | 282.8 / 289.6 / 385.3 | 57.4 / 56.5 / 56.5 | 4.82 / 5.30 / 5.67 | 1.02 / 1.00 / 1.00 |
| rtt60_policer5 | cubic | 41.2 / 41.0 / 41.1 | 132.6 / 186.1 / 200.2 | 212.0 / 202.8 / 186.2 | 110.9 / 111.5 / 112.2 | 245.6 / 236.9 / 228.9 | 60.0 / 59.5 / 58.3 | 4.23 / 4.03 / 3.91 | 1.26 / 2.18 / 1.96 |
| rtt60_policer5 | bbr | 40.7 / 40.6 / 40.6 | 172.0 / 168.7 / 196.6 | 253.5 / 185.2 / 260.5 | 157.8 / 154.0 / 164.8 | 294.2 / 231.9 / 303.2 | 59.8 / 60.3 / 58.2 | 4.25 / 4.28 / 4.25 | 2.90 / 2.13 / 4.36 |

With random loss, the receipts held BBR's rate at 4.8 to 8.2 Mbit/s and
Cubic's at 0.7 to 4.8, and at 120 ms and 1% BBR kept 56.5 to 57.4 FPS
against Cubic's 37.9 to 41.4. BBR's P-frame p99 and IDR wait p50 were the
lower in every run but two: its p99 in run 2 at 120 ms and 0.1% (303.7
against 280.2 ms), the run whose IDR p95 was 469 ms against 74 and 73 in
the others, and its IDR wait in run 1 at 120 ms and 1% (261.3 against
201.2 ms). Behind the
policer, BBR lost more packets in two runs of three (2.90% and 4.36%
against 1.26% and 1.96%, and 2.13% against 2.18%), its IDR wait p50 was
longer in every run (154 to 165 ms against 111 to 112), and its P-frame p99
was higher in one run (172.0 against 132.6 ms) and lower in two.

## Verdict

The loss half of the gate passed, the policer half did not: behind a
policer, with the receipts setting the rate, BBR lost more packets and
kept a requested keyframe waiting longer. The host keeps Cubic
(`VIDEO_CONGESTION` in `host.rs`); the choice stays one constant, and
`register_with_sender_congestion` and the link's `congestion` label stay
for the next measurement.

What the runs show of why: BBR's window ended at 14 576 762 bytes or more
in every closed-loop run and at 1 955 253 or more in every open-loop cell
without a policer, against Cubic's 15 762 to 73 234 in the closed loop.
Quinn paces from
the window (1.25 × window / smoothed RTT), so at that size nothing spaces a
burst: an IDR leaves at once, and a policer's bucket drops what it cannot
pay for, which the receipts see only as late frames. A BBR whose window,
or pacing, follows its delivery-rate estimate would be the variant to
measure next.

## What needs a device

A WAN direct session with real loss (cross-border or cellular), and one
behind a policed uplink: the host's `sent_bitrate`, pace and IDR wait in
`Stats`, under each controller. `transport_probe --auto --bbr` sends its
download under BBR, beside a run without `--bbr`, on the same path; the
desktop session needs `VIDEO_CONGESTION` switched in a build of its own.
