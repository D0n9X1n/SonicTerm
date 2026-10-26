//! Rasterize block keys with the shared cell metrics and preserved polygon geometry.

// Geometry from WezTerm 577474d89ee61aef4a48145cdec82a638d874751, MIT; see LICENSE-WEZTERM.
use super::*;

// Vendor change: the upstream `impl GlyphCache { fn draw_polys, fn
// cursor_sprite, fn block_sprite }` block is rewritten to free
// functions. See the header comment in customglyph.rs for the
// full rationale. The opening `impl GlyphCache {` brace is gone; the
// closing `}` at the original line 6007 is also gone (the standalone
// `fill_rect` helper that follows it stays as-is). `cursor_sprite` is
// dropped entirely — it is out of scope here (see customglyph.rs header §2).

fn draw_polys(
    metrics: &RenderMetrics,
    polys: &[Poly],
    buffer: &mut Image,
    anti_alias_mode: PolyAA,
    blend_mode: BlendMode,
) {
    let (width, height) = buffer.image_dimensions();
    let mut pixmap =
        PixmapMut::from_bytes(buffer.pixel_data_slice_mut(), width as u32, height as u32)
            .expect("make pixmap from existing bitmap");

    for Poly { path, intensity, style } in polys {
        let mut paint = Paint::default();
        paint.blend_mode = blend_mode;
        let intensity = intensity.to_scale();
        paint.set_color(
            tiny_skia::Color::from_rgba(intensity, intensity, intensity, intensity).unwrap(),
        );
        paint.anti_alias = match anti_alias_mode {
            PolyAA::AntiAlias => true,
            PolyAA::MoarPixels => false,
        };
        paint.force_hq_pipeline = true;
        let mut path_builder = PathBuilder::new();
        for item in path.iter() {
            item.to_skia(width, height, metrics.underline_height as f32, &mut path_builder);
        }
        if path_builder.is_empty() {
            // When: a collapsed circle leaves `path_builder` empty, it has no geometry to fill or clear.
            continue;
        }
        let path = path_builder.finish().expect("poly path to be valid");
        style.apply(metrics.underline_height as f32, &paint, &path, &mut pixmap);
    }
}

/// Rasterize a block key into a standalone tile using the supplied cell metrics and antialiasing.
pub fn block_sprite(
    render_metrics: &RenderMetrics,
    key: SizedBlockKey,
    anti_alias: bool,
) -> anyhow::Result<RasterTile> {
    let metrics = match &key.block {
        BlockKey::PolyWithCustomMetrics { underline_height, cell_size, .. } => RenderMetrics {
            descender: PixelLength::new(0.),
            descender_row: 0,
            descender_plus_two: 0,
            underline_height: *underline_height,
            strike_row: 0,
            cell_size: cell_size.clone(),
        },
        _ => render_metrics.clone(),
    };

    let mut buffer =
        Image::new(metrics.cell_size.width as usize, metrics.cell_size.height as usize);
    let black = SrgbaPixel::rgba(0, 0, 0, 0);

    let cell_rect = Rect::new(Point::new(0, 0), metrics.cell_size);

    buffer.clear_rect(cell_rect, black);

    match key.block {
        BlockKey::Blocks(blocks) => {
            let width = metrics.cell_size.width as f32;
            let height = metrics.cell_size.height as f32;
            let (x_half, y_half) = (width / 2., height / 2.);
            let (x_eighth, y_eighth) = (width / 8., height / 8.);

            for block in blocks.iter() {
                match block {
                    Block::Custom(
                        left_eighths,
                        right_eighths,
                        top_eighths,
                        bottom_eighths,
                        alpha,
                    ) => {
                        let left = (*left_eighths as f32) * x_eighth;
                        let right = (*right_eighths as f32) * x_eighth;
                        let top = (*top_eighths as f32) * y_eighth;
                        let bottom = (*bottom_eighths as f32) * y_eighth;
                        fill_rect(&mut buffer, left..right, top..bottom, *alpha);
                    }
                    Block::UpperBlock(num) => {
                        let lower = (*num as f32) * y_eighth;
                        fill_rect(&mut buffer, 0.0..width, 0.0..lower, BlockAlpha::Full);
                    }
                    Block::LowerBlock(num) => {
                        let upper = ((8 - num) as f32) * y_eighth;
                        fill_rect(&mut buffer, 0.0..width, upper..height, BlockAlpha::Full);
                    }
                    Block::LeftBlock(num) => {
                        let right = (*num as f32) * x_eighth;
                        fill_rect(&mut buffer, 0.0..right, 0.0..height, BlockAlpha::Full);
                    }
                    Block::RightBlock(num) => {
                        let left = ((8 - num) as f32) * x_eighth;
                        fill_rect(&mut buffer, left..width, 0.0..height, BlockAlpha::Full);
                    }
                    Block::VerticalBlock(left_eighths, right_eighths) => {
                        let left = (*left_eighths as f32) * x_eighth;
                        let right = (*right_eighths as f32) * x_eighth;
                        fill_rect(&mut buffer, left..right, 0.0..height, BlockAlpha::Full);
                    }
                    Block::HorizontalBlock(top_eighths, bottom_eighths) => {
                        let top = (*top_eighths as f32) * y_eighth;
                        let bottom = (*bottom_eighths as f32) * y_eighth;
                        fill_rect(&mut buffer, 0.0..width, top..bottom, BlockAlpha::Full);
                    }
                    Block::QuadrantUL => {
                        fill_rect(&mut buffer, 0.0..x_half, 0.0..y_half, BlockAlpha::Full)
                    }
                    Block::QuadrantUR => {
                        fill_rect(&mut buffer, x_half..width, 0.0..y_half, BlockAlpha::Full)
                    }
                    Block::QuadrantLL => {
                        fill_rect(&mut buffer, 0.0..x_half, y_half..height, BlockAlpha::Full)
                    }
                    Block::QuadrantLR => {
                        fill_rect(&mut buffer, x_half..width, y_half..height, BlockAlpha::Full)
                    }
                }
            }
        }
        BlockKey::Triangles(triangles, alpha) => {
            let mut draw = |cmd: &'static [PolyCommand], style: PolyStyle| {
                draw_polys(
                    &metrics,
                    &[Poly { path: cmd, intensity: alpha, style: style }],
                    &mut buffer,
                    if anti_alias { PolyAA::AntiAlias } else { PolyAA::MoarPixels },
                    BlendMode::default(),
                );
            };

            macro_rules! start {
                () => {
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2))
                };
            }
            macro_rules! close {
                () => {
                    PolyCommand::Close
                };
            }
            macro_rules! top_left {
                () => {
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Zero)
                };
            }
            macro_rules! top_right {
                () => {
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero)
                };
            }
            macro_rules! bottom_left {
                () => {
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One)
                };
            }
            macro_rules! bottom_right {
                () => {
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::One)
                };
            }

            // Draw triangles
            if triangles.contains(Triangle::UPPER) {
                draw(&[start!(), top_left!(), top_right!(), close!()], PolyStyle::Fill);
            }
            if triangles.contains(Triangle::LOWER) {
                draw(&[start!(), bottom_left!(), bottom_right!(), close!()], PolyStyle::Fill);
            }
            if triangles.contains(Triangle::LEFT) {
                draw(&[start!(), top_left!(), bottom_left!(), close!()], PolyStyle::Fill);
            }
            if triangles.contains(Triangle::RIGHT) {
                draw(&[start!(), top_right!(), bottom_right!(), close!()], PolyStyle::Fill);
            }

            // Fill antialiased lines between triangles
            let style = if alpha == BlockAlpha::Full {
                PolyStyle::Outline
            } else {
                PolyStyle::OutlineAlpha
            };
            if triangles.contains(Triangle::UPPER | Triangle::LEFT) {
                draw(&[start!(), top_left!()], style);
            }
            if triangles.contains(Triangle::UPPER | Triangle::RIGHT) {
                draw(&[start!(), top_right!()], style);
            }
            if triangles.contains(Triangle::LOWER | Triangle::LEFT) {
                draw(&[start!(), bottom_left!()], style);
            }
            if triangles.contains(Triangle::LOWER | Triangle::RIGHT) {
                draw(&[start!(), bottom_right!()], style);
            }
        }
        BlockKey::CellDiagonals(diagonals) => {
            let mut draw = |cmd: &'static [PolyCommand]| {
                draw_polys(
                    &metrics,
                    &[Poly { path: cmd, intensity: BlockAlpha::Full, style: PolyStyle::Outline }],
                    &mut buffer,
                    if anti_alias { PolyAA::AntiAlias } else { PolyAA::MoarPixels },
                    BlendMode::default(),
                );
            };

            macro_rules! top_middle {
                () => {
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero)
                };
            }
            macro_rules! bottom_middle {
                () => {
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One)
                };
            }
            macro_rules! left_middle {
                () => {
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2))
                };
            }
            macro_rules! right_middle {
                () => {
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2))
                };
            }

            if diagonals.contains(CellDiagonal::UPPER_LEFT) {
                draw(&[top_middle!(), left_middle!()]);
            }
            if diagonals.contains(CellDiagonal::UPPER_RIGHT) {
                draw(&[top_middle!(), right_middle!()]);
            }
            if diagonals.contains(CellDiagonal::LOWER_LEFT) {
                draw(&[bottom_middle!(), left_middle!()]);
            }
            if diagonals.contains(CellDiagonal::LOWER_RIGHT) {
                draw(&[bottom_middle!(), right_middle!()]);
            }
        }
        BlockKey::Sextant(pattern) => {
            let width = metrics.cell_size.width as f32;
            let height = metrics.cell_size.height as f32;
            let (x_half, y_third) = (width / 2., height / 3.);
            for row in 0..3 {
                for col in 0..2 {
                    let bit = 2 * row + col;
                    if pattern & (1u8 << bit) != 0 {
                        fill_rect(
                            &mut buffer,
                            col as f32 * x_half..(col + 1) as f32 * x_half,
                            row as f32 * y_third..(row + 1) as f32 * y_third,
                            BlockAlpha::Full,
                        );
                    }
                }
            }
        }
        BlockKey::Octant(pattern) => {
            let width = metrics.cell_size.width as f32;
            let height = metrics.cell_size.height as f32;
            let (x_half, y_fourth) = (width / 2., height / 4.);
            for row in 0..4 {
                for col in 0..2 {
                    let bit = 2 * row + col;
                    if pattern & (1u8 << bit) != 0 {
                        fill_rect(
                            &mut buffer,
                            col as f32 * x_half..(col + 1) as f32 * x_half,
                            row as f32 * y_fourth..(row + 1) as f32 * y_fourth,
                            BlockAlpha::Full,
                        );
                    }
                }
            }
        }
        BlockKey::Braille(dots_pattern) => {
            // When: Braille is selected, square dots preserve the fixed two-column, four-row layout.

            // `dots_pattern` is a byte whose bits corresponds to dots
            // on a 2 by 4 dots-grid.
            // The position of a dot for a bit position (1-indexed) is as follow:
            // 1 4  |
            // 2 5  |<- These 3 lines are filled first (for the first 64 symbols)
            // 3 6  |
            // 7 8  <- This last line is filled last (for the remaining 192 symbols)
            //
            // NOTE: for simplicity & performance reasons, a dot is a square not a circle.

            let dot_area_width = metrics.cell_size.width as f32 / 2.;
            let dot_area_height = metrics.cell_size.height as f32 / 4.;
            let square_length = dot_area_width / 2.;
            let topleft_offset_x = dot_area_width / 2. - square_length / 2.;
            let topleft_offset_y = dot_area_height / 2. - square_length / 2.;

            let (width, height) = buffer.image_dimensions();
            let mut pixmap =
                PixmapMut::from_bytes(buffer.pixel_data_slice_mut(), width as u32, height as u32)
                    .expect("make pixmap from existing bitmap");
            let mut paint = Paint::default();
            paint.set_color(tiny_skia::Color::WHITE);
            paint.force_hq_pipeline = true;
            paint.anti_alias = true;
            let identity = Transform::identity();

            const BIT_MASK_AND_DOT_POSITION: [(u8, f32, f32); 8] = [
                (1 << 0, 0., 0.),
                (1 << 1, 0., 1.),
                (1 << 2, 0., 2.),
                (1 << 3, 1., 0.),
                (1 << 4, 1., 1.),
                (1 << 5, 1., 2.),
                (1 << 6, 0., 3.),
                (1 << 7, 1., 3.),
            ];
            for (bit_mask, dot_pos_x, dot_pos_y) in &BIT_MASK_AND_DOT_POSITION {
                if dots_pattern & bit_mask == 0 {
                    // When: dots_pattern omits bit_mask, leave this dot transparent rather than painting it.

                    // Bit for this dot position is not set
                    continue;
                }
                let topleft_x = (*dot_pos_x) * dot_area_width + topleft_offset_x;
                let topleft_y = (*dot_pos_y) * dot_area_height + topleft_offset_y;

                let path = PathBuilder::from_rect(
                    tiny_skia::Rect::from_xywh(topleft_x, topleft_y, square_length, square_length)
                        .expect("valid rect"),
                );
                pixmap.fill_path(&path, &paint, FillRule::Winding, identity, None);
            }
        }
        BlockKey::Progress(chunks) => {
            let mut draw = |cmd: &'static [PolyCommand], style: PolyStyle| {
                draw_polys(
                    &metrics,
                    &[Poly { path: cmd, intensity: BlockAlpha::Full, style: style }],
                    &mut buffer,
                    if anti_alias { PolyAA::AntiAlias } else { PolyAA::MoarPixels },
                    BlendMode::default(),
                );
            };

            if chunks.contains(ProgressChunk::LEFT) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 6)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 6), BlockCoord::Frac(1, 6)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 6), BlockCoord::Frac(6 - 1, 6)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(6 - 1, 6)),
                    ],
                    PolyStyle::OutlineHeavy,
                );

                if chunks.contains(ProgressChunk::FULL) {
                    draw(
                        &[
                            PolyCommand::MoveTo(
                                BlockCoord::One,
                                BlockCoord::FracWithOffset(1, 6, LineScale::Mul(6)),
                            ),
                            PolyCommand::LineTo(
                                BlockCoord::FracWithOffset(1, 6, LineScale::Mul(6)),
                                BlockCoord::FracWithOffset(1, 6, LineScale::Mul(6)),
                            ),
                            PolyCommand::LineTo(
                                BlockCoord::FracWithOffset(1, 6, LineScale::Mul(6)),
                                BlockCoord::FracWithOffset(6 - 1, 6, LineScale::Mul(-6)),
                            ),
                            PolyCommand::LineTo(
                                BlockCoord::One,
                                BlockCoord::FracWithOffset(6 - 1, 6, LineScale::Mul(-6)),
                            ),
                            PolyCommand::Close,
                        ],
                        PolyStyle::Fill,
                    );
                }
            }
            if chunks.contains(ProgressChunk::RIGHT) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 6)),
                        PolyCommand::LineTo(BlockCoord::Frac(6 - 1, 6), BlockCoord::Frac(1, 6)),
                        PolyCommand::LineTo(BlockCoord::Frac(6 - 1, 6), BlockCoord::Frac(6 - 1, 6)),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(6 - 1, 6)),
                    ],
                    PolyStyle::OutlineHeavy,
                );

                if chunks.contains(ProgressChunk::FULL) {
                    draw(
                        &[
                            PolyCommand::MoveTo(
                                BlockCoord::Zero,
                                BlockCoord::FracWithOffset(1, 6, LineScale::Mul(6)),
                            ),
                            PolyCommand::LineTo(
                                BlockCoord::FracWithOffset(6 - 1, 6, LineScale::Mul(-6)),
                                BlockCoord::FracWithOffset(1, 6, LineScale::Mul(6)),
                            ),
                            PolyCommand::LineTo(
                                BlockCoord::FracWithOffset(6 - 1, 6, LineScale::Mul(-6)),
                                BlockCoord::FracWithOffset(6 - 1, 6, LineScale::Mul(-6)),
                            ),
                            PolyCommand::LineTo(
                                BlockCoord::Zero,
                                BlockCoord::FracWithOffset(6 - 1, 6, LineScale::Mul(-6)),
                            ),
                            PolyCommand::Close,
                        ],
                        PolyStyle::Fill,
                    );
                }
            }
            if chunks.contains(ProgressChunk::MIDDLE) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 6)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 6)),
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(6 - 1, 6)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(6 - 1, 6)),
                    ],
                    PolyStyle::OutlineHeavy,
                );

                if chunks.contains(ProgressChunk::FULL) {
                    draw(
                        &[
                            PolyCommand::MoveTo(
                                BlockCoord::Zero,
                                BlockCoord::FracWithOffset(1, 6, LineScale::Mul(6)),
                            ),
                            PolyCommand::LineTo(
                                BlockCoord::One,
                                BlockCoord::FracWithOffset(1, 6, LineScale::Mul(6)),
                            ),
                            PolyCommand::LineTo(
                                BlockCoord::One,
                                BlockCoord::FracWithOffset(6 - 1, 6, LineScale::Mul(-6)),
                            ),
                            PolyCommand::LineTo(
                                BlockCoord::Zero,
                                BlockCoord::FracWithOffset(6 - 1, 6, LineScale::Mul(-6)),
                            ),
                            PolyCommand::Close,
                        ],
                        PolyStyle::Fill,
                    );
                }
            }
        }
        BlockKey::Branches(pattern) => {
            let mut draw =
                |cmd: &'static [PolyCommand], style: PolyStyle, blend_mode: BlendMode| {
                    draw_polys(
                        &metrics,
                        &[Poly { path: cmd, intensity: BlockAlpha::Full, style: style }],
                        &mut buffer,
                        if anti_alias { PolyAA::AntiAlias } else { PolyAA::MoarPixels },
                        blend_mode,
                    );
                };

            if pattern.contains(Branch::VERTICAL) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    PolyStyle::OutlineHeavy,
                    BlendMode::default(),
                );
            }
            if pattern.contains(Branch::HORIZONTAL) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    PolyStyle::OutlineHeavy,
                    BlendMode::default(),
                );
            }
            if pattern.contains(Branch::RIGHT_TO_DOWN) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(7, 8)),
                        PolyCommand::QuadTo {
                            control: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            to: (BlockCoord::Frac(7, 8), BlockCoord::Frac(1, 2)),
                        },
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    PolyStyle::OutlineHeavy,
                    BlendMode::default(),
                );
            }
            if pattern.contains(Branch::LEFT_TO_DOWN) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(7, 8)),
                        PolyCommand::QuadTo {
                            control: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            to: (BlockCoord::Frac(1, 8), BlockCoord::Frac(1, 2)),
                        },
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    ],
                    PolyStyle::OutlineHeavy,
                    BlendMode::default(),
                );
            }
            if pattern.contains(Branch::RIGHT_TO_UP) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 8)),
                        PolyCommand::QuadTo {
                            control: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            to: (BlockCoord::Frac(7, 8), BlockCoord::Frac(1, 2)),
                        },
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    PolyStyle::OutlineHeavy,
                    BlendMode::default(),
                );
            }
            if pattern.contains(Branch::LEFT_TO_UP) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 8)),
                        PolyCommand::QuadTo {
                            control: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            to: (BlockCoord::Frac(1, 8), BlockCoord::Frac(1, 2)),
                        },
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    ],
                    PolyStyle::OutlineHeavy,
                    BlendMode::default(),
                );
            }
            if pattern.contains(Branch::LEFT) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    PolyStyle::OutlineHeavy,
                    BlendMode::default(),
                );
            }
            if pattern.contains(Branch::RIGHT) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    PolyStyle::OutlineHeavy,
                    BlendMode::default(),
                );
            }
            if pattern.contains(Branch::UP) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    PolyStyle::OutlineHeavy,
                    BlendMode::default(),
                );
            }
            if pattern.contains(Branch::DOWN) {
                draw(
                    &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    PolyStyle::OutlineHeavy,
                    BlendMode::default(),
                );
            }
            if pattern.contains(Branch::CIRCLE_FILLED) {
                draw(
                    &[PolyCommand::Circle {
                        center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        radius: BlockCoord::Frac(2, 5),
                    }],
                    PolyStyle::Fill,
                    BlendMode::default(),
                );
            }
            if pattern.contains(Branch::CIRCLE_OUTLINE) {
                draw(
                    &[PolyCommand::Circle {
                        center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        radius: BlockCoord::Frac(2, 5),
                    }],
                    PolyStyle::Fill,
                    BlendMode::default(),
                );
                draw(
                    &[PolyCommand::Circle {
                        center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        radius: BlockCoord::Frac(3, 10),
                    }],
                    PolyStyle::Fill,
                    BlendMode::Clear,
                );
            }
        }
        BlockKey::Spinner(segment) => {
            let mut draw =
                |cmd: &'static [PolyCommand], style: PolyStyle, blend_mode: BlendMode| {
                    draw_polys(
                        &metrics,
                        &[Poly { path: cmd, intensity: BlockAlpha::Full, style: style }],
                        &mut buffer,
                        if anti_alias { PolyAA::AntiAlias } else { PolyAA::MoarPixels },
                        blend_mode,
                    );
                };

            match segment {
                0 => {
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::Frac(1, 2),
                        }],
                        PolyStyle::Fill,
                        BlendMode::default(),
                    );
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-3)),
                        }],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                    draw(
                        &[
                            PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            PolyCommand::LineTo(BlockCoord::SquareOne, BlockCoord::SquareZero),
                            PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                            PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                            PolyCommand::LineTo(BlockCoord::SquareZero, BlockCoord::SquareZero),
                            PolyCommand::Close,
                        ],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                }
                1 => {
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::Frac(1, 2),
                        }],
                        PolyStyle::Fill,
                        BlendMode::default(),
                    );
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-3)),
                        }],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                    draw(
                        &[
                            PolyCommand::MoveTo(
                                BlockCoord::SquareFrac(1, 2),
                                BlockCoord::SquareFrac(1, 2),
                            ),
                            PolyCommand::LineTo(BlockCoord::SquareFrac(1, 2), BlockCoord::Zero),
                            PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Zero),
                            PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                            PolyCommand::LineTo(BlockCoord::SquareOne, BlockCoord::One),
                            PolyCommand::LineTo(BlockCoord::SquareOne, BlockCoord::SquareOne),
                            PolyCommand::Close,
                        ],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                }
                2 => {
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::Frac(1, 2),
                        }],
                        PolyStyle::Fill,
                        BlendMode::default(),
                    );
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-3)),
                        }],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                    draw(
                        &[
                            PolyCommand::MoveTo(
                                BlockCoord::SquareFrac(1, 2),
                                BlockCoord::SquareFrac(1, 2),
                            ),
                            PolyCommand::LineTo(BlockCoord::SquareOne, BlockCoord::SquareZero),
                            PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                            PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Zero),
                            PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                            PolyCommand::LineTo(
                                BlockCoord::SquareFrac(1, 3),
                                BlockCoord::SquareOne,
                            ),
                        ],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                }
                3 => {
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::Frac(1, 2),
                        }],
                        PolyStyle::Fill,
                        BlendMode::default(),
                    );
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-3)),
                        }],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                    draw(
                        &[
                            PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::SquareFrac(1, 2)),
                            PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Zero),
                            PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                            PolyCommand::LineTo(BlockCoord::One, BlockCoord::SquareFrac(1, 2)),
                        ],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                }
                4 => {
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::Frac(1, 2),
                        }],
                        PolyStyle::Fill,
                        BlendMode::default(),
                    );
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-3)),
                        }],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                    draw(
                        &[
                            PolyCommand::MoveTo(
                                BlockCoord::SquareFrac(1, 2),
                                BlockCoord::SquareFrac(1, 2),
                            ),
                            PolyCommand::LineTo(BlockCoord::SquareZero, BlockCoord::SquareZero),
                            PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Zero),
                            PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                            PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                            PolyCommand::LineTo(
                                BlockCoord::SquareFrac(2, 3),
                                BlockCoord::SquareOne,
                            ),
                        ],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                }
                5 => {
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::Frac(1, 2),
                        }],
                        PolyStyle::Fill,
                        BlendMode::default(),
                    );
                    draw(
                        &[PolyCommand::Circle {
                            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                            radius: BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-3)),
                        }],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                    draw(
                        &[
                            PolyCommand::MoveTo(
                                BlockCoord::SquareFrac(1, 2),
                                BlockCoord::SquareFrac(1, 2),
                            ),
                            PolyCommand::LineTo(BlockCoord::SquareFrac(1, 2), BlockCoord::Zero),
                            PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
                            PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
                            PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
                            PolyCommand::LineTo(BlockCoord::SquareZero, BlockCoord::SquareOne),
                            PolyCommand::Close,
                        ],
                        PolyStyle::Fill,
                        BlendMode::Clear,
                    );
                }
                _ => {
                    // When: segment has no spinner phase, the already-cleared tile remains transparent.
                }
            }
        }
        BlockKey::Poly(polys) | BlockKey::PolyWithCustomMetrics { polys, .. } => {
            draw_polys(
                &metrics,
                polys,
                &mut buffer,
                if anti_alias { PolyAA::AntiAlias } else { PolyAA::MoarPixels },
                BlendMode::default(),
            );
        }
    }

    /*
    log::info!("{:?}", block);
    buffer.log_bits();
    */

    // Vendor change: the wezterm-side `self.atlas.allocate` +
    // `self.block_glyphs.insert(...)` + `Ok(sprite)` 3-line tail
    // was removed from the body above and replaced with the
    // RasterTile pack below. The atlas / cache wrapping is the
    // consumer's job (see customglyph.rs header §2).
    let (width, height) = buffer.image_dimensions();
    Ok(RasterTile {
        width: width as u32,
        height: height as u32,
        offset_x: 0,
        offset_y: 0,
        advance: width as f32,
        coverage: buffer.into_bgra_vec(),
        is_color: true,
    })
}

// `fill_rect` (next) was the only standalone fn at upstream module
// level; it survives the impl-block deletion unchanged apart from the
// §6 parameter names.

// Fill a rectangular region described by the x and y ranges
fn fill_rect(buffer: &mut Image, x_range: Range<f32>, y_range: Range<f32>, intensity: BlockAlpha) {
    let (width, height) = buffer.image_dimensions();
    let mut pixmap =
        PixmapMut::from_bytes(buffer.pixel_data_slice_mut(), width as u32, height as u32)
            .expect("make pixmap from existing bitmap");

    let path = PathBuilder::from_rect(
        tiny_skia::Rect::from_xywh(
            x_range.start,
            y_range.start,
            x_range.end - x_range.start,
            y_range.end - y_range.start,
        )
        .expect("valid rect"),
    );

    let mut paint = Paint::default();
    let intensity = intensity.to_scale();
    paint.set_color(
        tiny_skia::Color::from_rgba(intensity, intensity, intensity, intensity).unwrap(),
    );
    paint.anti_alias = false;
    paint.force_hq_pipeline = true;

    pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

#[cfg(test)]
#[path = "customglyph_raster_tests.rs"]
mod customglyph_raster_tests;
