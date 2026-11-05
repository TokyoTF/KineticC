<p align="center">
    <img src="assets/README/kineticc-banner.png" alt="KineticC — Fork of ferrumC">
</p>


> [!NOTE]
> **KineticC is a fork of [FerrumC](https://github.com/ferrumc-rs/ferrumc).**
>
> This fork ([TokyoTF/ferrumc](https://github.com/TokyoTF/ferrumc), active development on the `v3` branch) keeps the upstream
> high-performance Rust core and focuses on **survival gameplay with vanilla-accurate behavior** for Minecraft **1.21.8**
> (protocol 772): mining and loot, block placement, neighbour updates, item physics and pickup delays — all verified against
> the decompiled vanilla 1.21.8 sources. In this fork the default gamemode is **survival**.

## 📖 About

KineticC is a fork of **FerrumC**, a **1.21.8** Minecraft server implementation written from the ground up in Rust. Leveraging
the power of the Rust programming language, it is completely multithreaded (Bevy ECS) and offers high performance as well as
amazing memory efficiency.

This fork turns the upstream creative/minigame core into a playable **survival server**: blocks are mined with vanilla
break times and drop their loot, placement follows the vanilla rules (fences connect, redstone wire joins, torches attach to
the right face, doors place as two halves...), and dropped items behave like vanilla — smooth falls, no erratic bouncing,
correct pickup delays.

Upstream website: **[ferrumc.com](https://www.ferrumc.com)** — documentation at **[docs.ferrumc.com](https://docs.ferrumc.com)**.

## 🔗 Project Links

* **KineticC (this fork):** **[TokyoTF/ferrumc](https://github.com/TokyoTF/ferrumc)**
* **Upstream FerrumC:** **[ferrumc-rs/ferrumc](https://github.com/ferrumc-rs/ferrumc)**
* **Official Website (upstream):** **[ferrumc.com](https://www.ferrumc.com)**
* **Documentation (upstream):** **[docs.ferrumc.com](https://docs.ferrumc.com)**

## 📜 License

This project is a fork of [FerrumC](https://github.com/ferrumc-rs/ferrumc) and is licensed under the MIT License — see the
[LICENSE](LICENSE) file for details. Upstream © [ferrumc-rs](https://github.com/ferrumc-rs/ferrumc).
