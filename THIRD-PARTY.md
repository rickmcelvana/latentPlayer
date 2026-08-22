# Third-party work this project builds on

latentPlayer itself is MIT (see `LICENSE`). This file records the outside work
whose *design or arithmetic* is reproduced here, as distinct from the crates
listed in `Cargo.toml` and `package.json`, which are linked rather than copied
and carry their own licences.

## MilkDrop 2 — BSD 3-clause, Copyright 2005–2013 Nullsoft, Inc.

`crates/player-render/src/milk/` implements the MilkDrop `.milk` preset format
(Phase 3 M7). Where behaviour had to match — the per-vertex warp transform, the
eight built-in waveform modes and their tessellation, the composite's video
echo, gamma and whole-frame filters, and the default value of every preset
scalar — it was read out of MilkDrop 2's own source rather than written from
memory, because a transform that is subtly wrong is invisible in a still frame
and obvious in motion.

That source is BSD 3-clause licensed. Its notice, as carried by every file in
`vis_milk2/`:

> Copyright 2005-2013 Nullsoft, Inc.
> All rights reserved.
>
> Redistribution and use in source and binary forms, with or without modification,
> are permitted provided that the following conditions are met:
>
>   * Redistributions of source code must retain the above copyright notice,
>     this list of conditions and the following disclaimer.
>
>   * Redistributions in binary form must reproduce the above copyright notice,
>     this list of conditions and the following disclaimer in the documentation
>     and/or other materials provided with the distribution.
>
>   * Neither the name of Nullsoft nor the names of its contributors may be used to
>     endorse or promote products derived from this software without specific prior written permission.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
> IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
> FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR
> CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
> DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
> DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER
> IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT
> OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

Nullsoft does not endorse this project, and nothing here should be read as
suggesting otherwise.

**This is *MilkDrop 2*, not MilkDrop 3.** The distinction is load-bearing: the
MilkDrop3 (milkdrop2077) repository declares no licence at all — no SPDX field
and no `LICENSE` file — so it is not a source anything here is derived from,
even though the presets used to test this build during development came from
it. Reading Nullsoft's BSD-licensed original is clean; assuming its successor
inherits that licence would not have been.

## EEL2 — Cockos WDL

`crates/player-render/src/milk/eel.rs` implements the expression language
MilkDrop presets are written in. Its two load-bearing quirks were verified
against Cockos' WDL source rather than from memory: `NSEEL_CLOSEFACTOR` is
`0.00001` and governs both `==`/`!=` and truthiness, and memory is
`NSEEL_RAM_BLOCKS` (128) blocks of `NSEEL_RAM_ITEMSPERBLOCK` (65536), reached
lazily. No WDL code is copied or linked.

## Butterchurn — MIT

Phase 3 decision 7 chose Butterchurn (MIT) over projectM (LGPL) as the guide
for `.milk` support. Guided rather than ported: Butterchurn transpiles EEL to
JavaScript and HLSL to GLSL through a fork of Unity's `hlslparser`, and neither
move is available to a Rust core that also has to run natively. What it
informed is the *format reading* — the part where a decade of real presets has
already found the corner cases, such as `.milk` code arriving one line per
numbered key and the two different ways those keys are numbered.

## Presets

**No `.milk` preset ships with this application** (Phase 3 decision 57, the
user's decision, 2026-08-22). Supporting the format is a licensing question
that has been answered; distributing someone else's preset is a separate one
that has not, and is not something this app needs to do. The preset library
points at a folder the user chooses.
