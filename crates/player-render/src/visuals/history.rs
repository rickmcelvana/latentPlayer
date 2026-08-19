//! A visual's own history, on the GPU: a ring of rows written on a clock.
//!
//! Extracted in M4, when the oscilloscope tunnel wanted exactly what the
//! spectrogram waterfall built in M3 -- the same rule `camera::fit` and
//! [`super::scale`] moved under, and the same reason: the *clock* in particular
//! is not a detail. Rows advance on elapsed seconds with the remainder carried,
//! never once per frame, because the desktop redraws at 60Hz and a browser's
//! animation callbacks run at whatever the display does (M3 decision 17). Two
//! visuals each keeping their own version of that arithmetic is two visuals that
//! eventually scroll at different speeds on the same machine.
//!
//! What a row *means* stays the visual's: the waterfall stores a spectrum row in
//! dBFS, the tunnel a cross-section of the waveform, one texel per point, two
//! components wide. This owns the texture, the head index, how much time is left
//! over, and nothing else -- it never decides what is drawn or when a history
//! stops being comparable to itself.

/// The ring, its clock, and the texture the two of them fill.
pub struct RingHistory {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Texels across. Rows are always written at the full width, which keeps a
    /// row's byte count constant and a multiple of 256 whatever a caller
    /// actually has to say (see [`Self::push`]).
    width: usize,
    /// Floats per texel: 1 for `R32Float`, 2 for `Rg32Float`.
    components: usize,
    rows: usize,
    /// What an unwritten texel means. Never zero by default for a reason worth
    /// keeping in front of a caller: on a dBFS axis zero is *full scale*, so a
    /// cleared texture read as levels would be a wall of it.
    empty: f32,
    head: usize,
    filled: usize,
    clock: RowClock,
    scratch: Vec<f32>,
}

/// When the next row is due, and how much time is left over towards it.
///
/// Its own type because it is the part with no GPU in it, and therefore the
/// part that can be checked as arithmetic -- which matters more than its four
/// lines suggest: this is the rule that makes a 60Hz redraw thread and a
/// 144Hz browser draw the same history.
#[derive(Debug)]
struct RowClock {
    seconds_per_row: f32,
    pending: f32,
    rows: usize,
}

impl RowClock {
    fn new(rows_per_second: f32, rows: usize) -> Self {
        Self {
            seconds_per_row: 1.0 / rows_per_second.max(f32::EPSILON),
            pending: 0.0,
            rows,
        }
    }

    fn due(&mut self, elapsed: f32) -> usize {
        let pending = self.pending + elapsed.max(0.0);
        if !pending.is_finite() || pending < self.seconds_per_row {
            self.pending = pending.max(0.0);
            return 0;
        }
        let whole = (pending / self.seconds_per_row).floor();
        self.pending = pending - whole * self.seconds_per_row;
        whole.min(self.rows as f32) as usize
    }

    fn fraction(&self) -> f32 {
        (self.pending / self.seconds_per_row).clamp(0.0, 1.0)
    }
}

impl RingHistory {
    pub fn new(
        device: &wgpu::Device,
        label: &str,
        format: wgpu::TextureFormat,
        width: usize,
        rows: usize,
        rows_per_second: f32,
        empty: f32,
    ) -> Self {
        let components = match format {
            wgpu::TextureFormat::R32Float => 1,
            wgpu::TextureFormat::Rg32Float => 2,
            other => unreachable!("history textures are 1 or 2 float channels, not {other:?}"),
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: width as u32,
                height: rows as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            width,
            components,
            rows,
            empty,
            head: rows.saturating_sub(1),
            filled: 0,
            clock: RowClock::new(rows_per_second, rows),
            scratch: vec![empty; width * components],
        }
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Which texture row holds the newest measurement.
    pub fn head(&self) -> usize {
        self.head
    }

    /// How many rows have been written since the last clear, capped at the
    /// ring's length. Zero means nothing has ever been measured, which every
    /// visual here draws as *nothing at all* rather than as a floor.
    pub fn filled(&self) -> usize {
        self.filled
    }

    /// Time carried towards the next row, as a fraction of one.
    ///
    /// A visual that moves *between* rows needs this: the tunnel's camera
    /// glides where its rings arrive in steps, and without the fraction it
    /// would judder at exactly the rate the rings do.
    pub fn fraction(&self) -> f32 {
        self.clock.fraction()
    }

    /// Empties the ring.
    pub fn clear(&mut self, queue: &wgpu::Queue) {
        let blank = vec![self.empty; self.width * self.components * self.rows];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&blank),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.row_bytes()),
                rows_per_image: Some(self.rows as u32),
            },
            wgpu::Extent3d {
                width: self.width as u32,
                height: self.rows as u32,
                depth_or_array_layers: 1,
            },
        );
        self.head = self.rows.saturating_sub(1);
        self.filled = 0;
        self.clock.pending = 0.0;
    }

    /// Writes `row` as the newest row.
    ///
    /// Short input is padded with [`Self::empty`] and long input is ignored past
    /// the width: a row always costs the same bytes, which is both one less
    /// thing to change when a caller's own width does and -- since the widths
    /// here are chosen so that `width * components * 4` is a multiple of 256 --
    /// clear of every alignment rule a texture copy has.
    ///
    /// Values that are not finite are stored as `empty`. A stored NaN would come
    /// back out of `textureLoad` and into a clamp whose answer is nobody's
    /// business to rely on.
    pub fn push(&mut self, queue: &wgpu::Queue, row: &[f32]) {
        self.scratch.fill(self.empty);
        for (slot, value) in self.scratch.iter_mut().zip(row) {
            *slot = if value.is_finite() {
                *value
            } else {
                self.empty
            };
        }
        self.head = (self.head + 1) % self.rows.max(1);
        self.filled = (self.filled + 1).min(self.rows);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: self.head as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&self.scratch),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.row_bytes()),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: self.width as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Writes `row` into **every** row of the ring at once.
    ///
    /// For a visual whose picture needs a whole history before it is a picture
    /// at all -- the tunnel is a tube seen end-on, and two rings of it subtend
    /// nothing. One measurement repeated is an honest opening shape ("this is
    /// what the sound is doing"), and four seconds of real history replaces it
    /// as it arrives.
    pub fn fill(&mut self, queue: &wgpu::Queue, row: &[f32]) {
        self.scratch.fill(self.empty);
        for (slot, value) in self.scratch.iter_mut().zip(row) {
            *slot = if value.is_finite() {
                *value
            } else {
                self.empty
            };
        }
        let mut every = Vec::with_capacity(self.scratch.len() * self.rows);
        for _ in 0..self.rows {
            every.extend_from_slice(&self.scratch);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&every),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.row_bytes()),
                rows_per_image: Some(self.rows as u32),
            },
            wgpu::Extent3d {
                width: self.width as u32,
                height: self.rows as u32,
                depth_or_array_layers: 1,
            },
        );
        self.head = self.rows.saturating_sub(1);
        self.filled = self.rows;
    }

    /// How many whole rows `elapsed` seconds are worth, carrying the remainder
    /// (M3 decision 17).
    ///
    /// The carry is what makes this a clock rather than a frame counter: sixty
    /// frames of a sixtieth of a second move the history exactly as far as one
    /// frame of a whole second. The cap is the other half -- a gap longer than
    /// the history is a whole new history, and a machine resumed from sleep
    /// should arrive there in one frame rather than by writing every row it
    /// slept through.
    pub fn rows_due(&mut self, elapsed: f32) -> usize {
        self.clock.due(elapsed)
    }

    fn row_bytes(&self) -> u32 {
        (self.width * self.components * 4) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_advance_on_the_clock_and_not_on_frames() {
        let mut one_go = RowClock::new(20.0, 120);
        assert_eq!(one_go.due(1.0), 20);
        assert!(one_go.pending.abs() < 1e-4);

        // The same second in sixty short frames, which is the difference
        // between the two shells' redraw loops.
        let mut stepwise = RowClock::new(20.0, 120);
        let total: usize = (0..60).map(|_| stepwise.due(1.0 / 60.0)).sum();
        assert_eq!(total, 20, "sixty frames moved {total} rows");

        // A frame shorter than a row moves nothing and keeps the remainder,
        // which is what stops a fast display from rounding time away.
        let mut fast = RowClock::new(20.0, 120);
        assert_eq!(fast.due(1.0 / 80.0), 0);
        assert!((fast.fraction() - 0.25).abs() < 1e-4);

        // And a machine that was asleep arrives at a whole new history in one
        // frame rather than writing every row it slept through.
        let mut resumed = RowClock::new(20.0, 120);
        assert_eq!(resumed.due(600.0), 120);
    }

    #[test]
    fn the_fraction_runs_from_one_row_to_the_next() {
        // What the tunnel's camera glides on: it has to reach neither less than
        // zero nor more than one, whatever it is handed, or the eye leaves the
        // tube between two rings.
        let mut clock = RowClock::new(60.0, 240);
        assert_eq!(clock.fraction(), 0.0);
        clock.due(1.0 / 120.0);
        assert!((clock.fraction() - 0.5).abs() < 1e-4);
        clock.due(-1.0);
        assert!((0.0..=1.0).contains(&clock.fraction()));
        clock.due(f32::NAN);
        assert!((0.0..=1.0).contains(&clock.fraction()));
    }
}
