//! Single-line box-drawing tees and crosses with mixed stroke weights.

// Geometry from WezTerm 577474d89ee61aef4a48145cdec82a638d874751, MIT; see LICENSE-WEZTERM.
use super::*;

impl BlockKey {
    /// Map the codepoints owned by this glyph family without changing their geometry.
    pub(super) fn from_box_tees_crosses(codepoint: u32) -> Option<Self> {
        Some(match codepoint {
            // [├] BOX DRAWINGS LIGHT VERTICAL AND RIGHT
            0x251c => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [┝] BOX DRAWINGS LIGHT VERTICAL LIGHT AND RIGHT HEAVY
            0x251d => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [┞] BOX DRAWINGS UP HEAVY and RIGHT DOWN LIGHT
            0x251e => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [┟] BOX DRAWINGS DOWN HEAVY and RIGHT UP LIGHT
            0x251f => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),

            // [┠] BOX DRAWINGS HEAVY VERTICAL and RIGHT LIGHT
            0x2520 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [┡] BOX DRAWINGS DOWN LIGHT AND RIGHT UP HEAVY
            0x2521 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [┢] BOX DRAWINGS UP LIGHT AND RIGHT DOWN HEAVY
            0x2522 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [┣] BOX DRAWINGS HEAVY VERTICAL and RIGHT
            0x2523 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [┤] BOX DRAWINGS LIGHT VERTICAL and LEFT
            0x2524 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [┥] BOX DRAWINGS VERTICAL LIGHT and LEFT HEAVY
            0x2525 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [┦] BOX DRAWINGS UP HEAVY and LEFT DOWN LIGHT
            0x2526 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [┧] BOX DRAWINGS DOWN HEAVY and LEFT UP LIGHT
            0x2527 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [┨] BOX DRAWINGS VERTICAL HEAVY and LEFT LIGHT
            0x2528 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [┩] BOX DRAWINGS DOWN LIGHT and LEFT UP HEAVY
            0x2529 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [┪] BOX DRAWINGS UP LIGHT and LEFT DOWN HEAVY
            0x252a => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [┫] BOX DRAWINGS HEAVY VERTICAL and LEFT
            0x252b => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [┬] BOX DRAWINGS LIGHT DOWN AND HORIZONTAL
            0x252c => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [┭] BOX DRAWINGS LEFT HEAVY AND RIGHT DOWN LIGHT
            0x252d => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [┮] BOX DRAWINGS RIGHT HEAVY AND LEFT DOWN LIGHT
            0x252e => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [┯] BOX DRAWINGS DOWN LIGHT AND HORIZONTAL HEAVY
            0x252f => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),

            // [┰] BOX DRAWINGS DOWN HEAVY AND HORIZONTAL LIGHT
            0x2530 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),

            // [┱] BOX DRAWINGS RIGHT LIGHT AND LEFT DOWN HEAVY
            0x2531 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [┲] BOX DRAWINGS LEFT LIGHT AND RIGHT DOWN HEAVY
            0x2532 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [┳] BOX DRAWINGS HEAVY DOWN AND HORIZONTAL
            0x2533 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [┴] BOX DRAWINGS LIGHT UP AND HORIZONTAL
            0x2534 => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [┵] BOX DRAWINGS LEFT HEAVY AND RIGHT UP LIGHT
            0x2535 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [┶] BOX DRAWINGS RIGHT HEAVY AND LEFT UP LIGHT
            0x2536 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [┷] BOX DRAWINGS UP LIGHT AND HORIZONTAL HEAVY
            0x2537 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),

            // [┸] BOX DRAWINGS UP HEAVY AND HORIZONTAL LIGHT
            0x2538 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),

            // [┹] BOX DRAWINGS RIGHT LIGHT AND LEFT UP HEAVY
            0x2539 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [┺] BOX DRAWINGS LEFT LIGHT AND RIGHT UP HEAVY
            0x253a => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [┻] BOX DRAWINGS HEAVY UP AND HORIZONTAL
            0x253b => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),
            // [┼] BOX DRAWINGS LIGHT VERTICAL AND HORIZONTAL
            0x253c => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::Outline,
            }]),
            // [┽] BOX DRAWINGS LEFT HEAVY AND RIGHT VERTICAL LIGHT
            0x253d => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [┾] BOX DRAWINGS RIGHT HEAVY AND LEFT VERTICAL LIGHT
            0x253e => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [┿] BOX DRAWINGS VERTICAL LIGHT AND HORIZONTAL HEAVY
            0x253f => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [╀] BOX DRAWINGS UP HEAVY AND DOWN HORIZONTAL LIGHT
            0x2540 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╁] BOX DRAWINGS DOWN HEAVY AND UP HORIZONTAL LIGHT
            0x2541 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╂] BOX DRAWINGS VERTICAL HEAVY AND HORIZONTAL LIGHT
            0x2542 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╃] BOX DRAWINGS LEFT UP HEAVY and RIGHT DOWN LIGHT
            0x2543 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╄] BOX DRAWINGS RIGHT UP HEAVY and LEFT DOWN LIGHT
            0x2544 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╅] BOX DRAWINGS LEFT DOWN HEAVY and RIGHT UP LIGHT
            0x2545 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╆] BOX DRAWINGS RIGHT DOWN HEAVY and LEFT UP LIGHT
            0x2546 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
            ]),
            // [╇] BOX DRAWINGS DOWN LIGHT AND UP HORIZONTAL HEAVY
            0x2547 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [╈] BOX DRAWINGS UP LIGHT AND DOWN HORIZONTAL HEAVY
            0x2548 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [╉] BOX DRAWINGS RIGHT LIGHT AND LEFT VERTICAL HEAVY
            0x2549 => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [╊] BOX DRAWINGS LEFT LIGHT AND RIGHT VERTICAL HEAVY
            0x254a => Self::Poly(&[
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::Outline,
                },
                Poly {
                    path: &[
                        PolyCommand::MoveTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                        PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                    ],
                    intensity: BlockAlpha::Full,
                    style: PolyStyle::OutlineHeavy,
                },
            ]),
            // [╋] BOX DRAWINGS HEAVY VERTICAL AND HORIZONTAL
            0x254b => Self::Poly(&[Poly {
                path: &[
                    PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Frac(1, 2)),
                    PolyCommand::LineTo(BlockCoord::One, BlockCoord::Frac(1, 2)),
                    PolyCommand::MoveTo(BlockCoord::Frac(1, 2), BlockCoord::Zero),
                    PolyCommand::LineTo(BlockCoord::Frac(1, 2), BlockCoord::One),
                ],
                intensity: BlockAlpha::Full,
                style: PolyStyle::OutlineHeavy,
            }]),

            _ => {
                // When: codepoint has no geometry in this family, preserve the unsupported result.
                return None;
            }
        })
    }
}
