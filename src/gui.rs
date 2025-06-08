//! Functions to draw our data into an `egui` context
use std::collections::HashMap;

use crate::{
    view::{
        RenderMode, RenderSettings, ViewCanvas, ViewData, ViewMode2, ViewMode3,
    },
    world::{Block, BlockError, BlockIndex, IoValue, NameError, World},
    BlockResponse, Message, ViewResponse,
};
use serde::{Deserialize, Serialize};

pub struct WorldView<'a> {
    pub world: &'a mut World,
    pub syntax: &'a egui_extras::syntax_highlighting::SyntectSettings,
    pub views: &'a mut HashMap<BlockIndex, ViewData>,
    pub out: &'a mut Vec<(BlockIndex, ViewResponse)>,
    pub tx: &'a std::sync::mpsc::Sender<Message>,
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
enum TabMode {
    Script,
    View,
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    index: BlockIndex,
    mode: TabMode,
}

impl Tab {
    pub fn script(index: BlockIndex) -> Self {
        Self {
            index,
            mode: TabMode::Script,
        }
    }
    pub fn view(index: BlockIndex) -> Self {
        Self {
            index,
            mode: TabMode::View,
        }
    }
}

impl<'a> egui_dock::TabViewer for WorldView<'a> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        tab.index.id().with(match tab.mode {
            TabMode::Script => "tab_script",
            TabMode::View => "tab_view",
        })
    }

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        let mut name = self.world[tab.index].name.to_string();
        match tab.mode {
            TabMode::Script => (),
            TabMode::View => name += " (view)",
        };
        egui::WidgetText::from(&name)
    }

    /// Draw a dock tab as either a script or view pane
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        let r = match tab.mode {
            TabMode::Script => self.script_ui(ui, tab.index),
            TabMode::View => self.view_ui(ui, tab.index),
        };
        if !r.is_empty() {
            self.out.push((tab.index, r))
        }
    }
}

impl<'a> WorldView<'a> {
    fn view_ui(
        &mut self,
        ui: &mut egui::Ui,
        index: BlockIndex,
    ) -> ViewResponse {
        // Is the block valid?
        // Does the block have a current view?
        // Does the view list have an entry for this block?
        // Does that entry have a valid image?
        //
        // Many things to consider...
        let mut out = ViewResponse::empty();
        let block = &self.world[index];
        let block_view = block.get_view();
        let rect = ui.clip_rect();
        let size = fidget::render::ImageSize::new(
            rect.width() as u32,
            rect.height() as u32,
        );
        let entry = self.views.entry(index);
        // If the block does not define a view, and there is no previous view
        // associated with this block, then we can't do anything.
        if block_view.is_none()
            && matches!(entry, std::collections::hash_map::Entry::Vacant(..))
        {
            return out
                | Self::view_fallback_ui(
                    ui,
                    index,
                    None,
                    size,
                    ERROR,
                    Some("block has errors and no previous view"),
                );
        }

        // At this point, either we have a block view, or we have a previous
        // view from this block.  We'll either get the previous entry or insert
        // a new empty one.
        let entry = entry.or_insert_with(|| ViewData::new(size));

        let ctx = ui.ctx().clone();
        // If we have a block view, then use it (or fall back to the previous
        // image, drawing it in a valid state).  Otherwise, fall back to the
        // previous image, drawing it in an *invalid* state (with a red border).
        let current_canvas = entry.canvas;
        let (image, valid) = if let Some(block_view) = block_view {
            let mode = match &entry.canvas {
                ViewCanvas::Canvas2 { canvas, mode } => RenderMode::Render2 {
                    view: canvas.view(),
                    size,
                    mode: *mode,
                },
                ViewCanvas::Canvas3 { canvas, mode } => {
                    RenderMode::Render3 {
                        view: canvas.view(),
                        size: fidget::render::VoxelSize::new(
                            size.width(),
                            size.height(),
                            // XXX select depth?
                            size.width().max(size.height()),
                        ),
                        mode: *mode,
                    }
                }
            };
            let settings = RenderSettings {
                tree: block_view.tree.clone(),
                mode,
            };

            let notify = move || ctx.request_repaint();

            let Some(image) =
                entry.image(index, settings, self.tx.clone(), notify)
            else {
                return out
                    | Self::view_fallback_ui(
                        ui,
                        index,
                        Some(entry),
                        size,
                        HOURGLASS,
                        None,
                    );
            };
            (image, true)
        } else if let Some(prev_image) = entry.prev_image() {
            (prev_image, false)
        } else {
            // XXX can we actually get here?
            return out
                | Self::view_fallback_ui(
                    ui,
                    index,
                    Some(entry),
                    size,
                    ERROR,
                    Some("block has errors and no previous image"),
                );
        };

        // This is the magic that triggers the GPU callback.  We pick a render
        // mode based on the selected image's settings
        match (image.settings.mode, current_canvas) {
            (
                RenderMode::Render2 {
                    mode: ViewMode2::Bitfield,
                    ..
                },
                ViewCanvas::Canvas2 {
                    mode: ViewMode2::Bitfield,
                    canvas,
                },
            ) => {
                ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                    rect,
                    crate::painters::WgpuBitfieldPainter::new(
                        index,
                        image.clone(),
                        size,
                        canvas.view(),
                    ),
                ));
            }
            (
                RenderMode::Render2 {
                    mode: ViewMode2::Sdf,
                    ..
                },
                ViewCanvas::Canvas2 {
                    mode: ViewMode2::Sdf,
                    canvas,
                },
            ) => {
                ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                    rect,
                    crate::painters::WgpuSdfPainter::new(
                        index,
                        image.clone(),
                        size,
                        canvas.view(),
                    ),
                ));
            }
            (
                RenderMode::Render3 {
                    mode: image_mode, ..
                },
                ViewCanvas::Canvas3 {
                    mode: canvas_mode,
                    canvas,
                },
            ) if image_mode == canvas_mode => {
                ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                    rect,
                    crate::painters::WgpuHeightmapPainter::new(
                        index,
                        image.clone(),
                        size,
                        canvas.view(),
                    ),
                ));
            }
            _ => {
                return out
                    | if entry.task.as_ref().is_some_and(|t| !t.done()) {
                        Self::view_fallback_ui(
                            ui,
                            index,
                            Some(entry),
                            size,
                            HOURGLASS,
                            None,
                        )
                    } else {
                        Self::view_fallback_ui(
                            ui,
                            index,
                            Some(entry),
                            size,
                            ERROR,
                            Some(
                                "block has errors and no previous \
                                 image in this mode",
                            ),
                        )
                    }
            }
        }

        let r = ui.interact(
            rect,
            index.id().with("block_view_interact"),
            egui::Sense::click_and_drag(),
        );

        // Send mouse interactions to the canvas
        let render_changed = match &mut entry.canvas {
            ViewCanvas::Canvas2 { canvas, .. } => {
                let cursor_state =
                    match (r.interact_pointer_pos(), r.hover_pos()) {
                        (Some(p), _) => Some((p, true)),
                        (_, Some(p)) => Some((p, false)),
                        (None, None) => None,
                    }
                    .map(|(p, drag)| {
                        let p = p - rect.min;
                        fidget::gui::CursorState {
                            screen_pos: nalgebra::Point2::new(
                                p.x.round() as i32,
                                p.y.round() as i32,
                            ),
                            drag,
                        }
                    });
                canvas.interact(
                    size,
                    cursor_state,
                    if r.hover_pos().is_some() {
                        ui.ctx().input(|i| i.smooth_scroll_delta.y)
                    } else {
                        0.0
                    },
                )
            }
            ViewCanvas::Canvas3 { canvas, .. } => {
                let size = fidget::render::VoxelSize::new(
                    size.width(),
                    size.height(),
                    size.width().max(size.height()),
                );
                let cursor_state =
                    match (r.interact_pointer_pos(), r.hover_pos()) {
                        (Some(p), _) => {
                            let drag =
                                if r.dragged_by(egui::PointerButton::Primary) {
                                    Some(fidget::gui::DragMode::Pan)
                                } else if r
                                    .dragged_by(egui::PointerButton::Secondary)
                                {
                                    Some(fidget::gui::DragMode::Rotate)
                                } else {
                                    None
                                };

                            Some((p, drag))
                        }
                        (_, Some(p)) => Some((p, None)),
                        (None, None) => None,
                    }
                    .map(|(p, drag)| {
                        let p = p - rect.min;
                        fidget::gui::CursorState {
                            screen_pos: nalgebra::Point2::new(
                                p.x.round() as i32,
                                p.y.round() as i32,
                            ),
                            drag,
                        }
                    });

                canvas.interact(
                    size,
                    cursor_state,
                    ui.ctx().input(|i| i.smooth_scroll_delta.y),
                )
            }
        };
        if render_changed {
            out |= ViewResponse::REDRAW;
        }

        if valid {
            out |= Self::view_edit_button(ui, index, entry, size);
        } else {
            ui.painter().rect_stroke(
                rect,
                0.0,
                egui::Stroke {
                    width: 4.0,
                    color: ui.style().visuals.error_fg_color,
                },
                egui::StrokeKind::Inside,
            );
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::TOP),
                |ui| {
                    let r = ui
                        .add(
                            egui::Label::new(
                                egui::RichText::new(WARN)
                                    .color(egui::Color32::WHITE)
                                    .background_color(
                                        ui.style().visuals.error_fg_color,
                                    ),
                            )
                            .sense(egui::Sense::CLICK),
                        )
                        .on_hover_ui(|ui| {
                            ui.label("script contains errors");
                        });
                    if r.clicked() {
                        out |= ViewResponse::FOCUS_ERR;
                    }
                    ui.with_layout(
                        egui::Layout::left_to_right(egui::Align::TOP),
                        |ui| {
                            out |=
                                Self::view_edit_button(ui, index, entry, size);
                        },
                    );
                },
            );
        }
        out
    }

    fn view_edit_button(
        ui: &mut egui::Ui,
        index: BlockIndex,
        entry: &mut ViewData,
        size: fidget::render::ImageSize,
    ) -> ViewResponse {
        let mut out = ViewResponse::empty();
        // Pop-up box to change render settings
        #[derive(Copy, Clone, PartialEq, Eq)]
        enum ViewCanvasType {
            Sdf,
            Bitfield,
            Heightmap,
            Shaded,
        }
        let initial_tag = match &entry.canvas {
            ViewCanvas::Canvas2 {
                mode: ViewMode2::Bitfield,
                ..
            } => ViewCanvasType::Bitfield,
            ViewCanvas::Canvas2 {
                mode: ViewMode2::Sdf,
                ..
            } => ViewCanvasType::Sdf,
            ViewCanvas::Canvas3 {
                mode: ViewMode3::Heightmap,
                ..
            } => ViewCanvasType::Heightmap,
            ViewCanvas::Canvas3 {
                mode: ViewMode3::Shaded,
                ..
            } => ViewCanvasType::Shaded,
        };
        let mut tag = initial_tag;
        let mut reset_camera = false;
        egui::ComboBox::from_id_salt(index.id().with("view_editor"))
            .selected_text(CAMERA)
            .width(0.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut tag,
                    ViewCanvasType::Bitfield,
                    "2D bitfield",
                );
                ui.selectable_value(&mut tag, ViewCanvasType::Sdf, "2D SDF");
                ui.separator();
                ui.selectable_value(
                    &mut tag,
                    ViewCanvasType::Heightmap,
                    "3D heightmap",
                );
                ui.selectable_value(
                    &mut tag,
                    ViewCanvasType::Shaded,
                    "3D shaded",
                );
                ui.separator();
                if ui.button("Reset camera").clicked() {
                    reset_camera = true;
                }
            });
        // If we've edited the canvas tag, then update it in the entry
        if tag != initial_tag {
            out |= ViewResponse::REDRAW;
            let mut next_canvas = match tag {
                ViewCanvasType::Sdf | ViewCanvasType::Bitfield => {
                    ViewCanvas::Canvas2 {
                        canvas: fidget::gui::Canvas2::new(size),
                        mode: match tag {
                            ViewCanvasType::Sdf => ViewMode2::Sdf,
                            ViewCanvasType::Bitfield => ViewMode2::Bitfield,
                            _ => unreachable!(),
                        },
                    }
                }
                ViewCanvasType::Heightmap | ViewCanvasType::Shaded => {
                    let size = fidget::render::VoxelSize::new(
                        size.width(),
                        size.height(),
                        size.width().max(size.height()), // XXX select depth?
                    );
                    ViewCanvas::Canvas3 {
                        canvas: fidget::gui::Canvas3::new(size),
                        mode: match tag {
                            ViewCanvasType::Heightmap => ViewMode3::Heightmap,
                            ViewCanvasType::Shaded => ViewMode3::Shaded,
                            _ => unreachable!(),
                        },
                    }
                }
            };
            match (&mut next_canvas, &mut entry.canvas) {
                (
                    ViewCanvas::Canvas2 {
                        canvas: next_canvas,
                        ..
                    },
                    ViewCanvas::Canvas2 {
                        canvas: prev_canvas,
                        ..
                    },
                ) => std::mem::swap(next_canvas, prev_canvas),
                (
                    ViewCanvas::Canvas3 {
                        canvas: next_canvas,
                        ..
                    },
                    ViewCanvas::Canvas3 {
                        canvas: prev_canvas,
                        ..
                    },
                ) => std::mem::swap(next_canvas, prev_canvas),
                _ => (), // TODO do some swapping if we do 2D <-> 3D?
            }
            entry.canvas = next_canvas;
        }
        if reset_camera {
            match &mut entry.canvas {
                ViewCanvas::Canvas2 { canvas, .. } => {
                    *canvas = fidget::gui::Canvas2::new(canvas.image_size());
                    out |= ViewResponse::REDRAW;
                }
                ViewCanvas::Canvas3 { canvas, .. } => {
                    *canvas = fidget::gui::Canvas3::new(canvas.image_size());
                    out |= ViewResponse::REDRAW;
                }
            }
        }
        out
    }

    /// Manually draw a backdrop indicating that the view is invalid
    fn view_fallback_ui(
        ui: &mut egui::Ui,
        index: BlockIndex,
        entry: Option<&mut ViewData>,
        size: fidget::render::ImageSize,
        inner_text: &str,
        error_text: Option<&str>,
    ) -> ViewResponse {
        let mut out = ViewResponse::empty();

        let style = ui.style();
        let painter = ui.painter();

        let mut t = style.text_styles[&egui::TextStyle::Heading].clone();
        t.size *= 2.0;
        let layout = painter.layout(
            inner_text.to_owned(),
            t,
            style.visuals.widgets.noninteractive.text_color(),
            f32::INFINITY,
        );
        let rect = painter.clip_rect();
        let text_corner = rect.center() - layout.size() / 2.0;
        painter.rect_filled(rect, 0.0, style.visuals.panel_fill);
        painter.galley(text_corner, layout, egui::Color32::BLACK);

        if let Some(error_text) = error_text {
            ui.painter().rect_stroke(
                rect,
                0.0,
                egui::Stroke {
                    width: 4.0,
                    color: ui.style().visuals.error_fg_color,
                },
                egui::StrokeKind::Inside,
            );
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::TOP),
                |ui| {
                    let r = ui
                        .add(
                            egui::Label::new(
                                egui::RichText::new(WARN)
                                    .color(egui::Color32::WHITE)
                                    .background_color(
                                        ui.style().visuals.error_fg_color,
                                    ),
                            )
                            .sense(egui::Sense::CLICK),
                        )
                        .on_hover_ui(|ui| {
                            ui.label(error_text);
                        });
                    if r.clicked() {
                        out |= ViewResponse::FOCUS_ERR;
                    }
                    if let Some(entry) = entry {
                        ui.with_layout(
                            egui::Layout::left_to_right(egui::Align::TOP),
                            |ui| {
                                out |= Self::view_edit_button(
                                    ui, index, entry, size,
                                );
                            },
                        );
                    }
                },
            );
        } else if let Some(entry) = entry {
            out |= Self::view_edit_button(ui, index, entry, size);
        }
        out
    }

    fn script_ui(
        &mut self,
        ui: &mut egui::Ui,
        index: BlockIndex,
    ) -> ViewResponse {
        let block = &mut self.world[index];
        let mut out = ViewResponse::empty();
        let theme =
            egui_extras::syntax_highlighting::CodeTheme::from_style(ui.style());
        let out = ui
            .with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                let mut line_count = block.script.lines().count();
                if block.script.is_empty() || block.script.ends_with('\n') {
                    line_count += 1;
                }
                let mut line_text = (1..=line_count)
                    .map(|i| i.to_string())
                    .collect::<Vec<String>>()
                    .join("\n");
                let max_indent = line_count.to_string().len();
                let width = max_indent as f32
                    * ui.text_style_height(&egui::TextStyle::Monospace)
                    * 0.5;
                ui.add(
                    egui::TextEdit::multiline(&mut line_text)
                        .id_source(index.id().with("line_numbers"))
                        .font(egui::TextStyle::Monospace)
                        .interactive(false)
                        .desired_width(width)
                        .frame(false),
                );

                let mut layouter =
                    |ui: &egui::Ui, buf: &str, wrap_width: f32| {
                        let mut layout_job =
                            egui_extras::syntax_highlighting::highlight_with(
                                ui.ctx(),
                                ui.style(),
                                &theme,
                                buf,
                                "rhai",
                                self.syntax,
                            );
                        layout_job.wrap.max_width = wrap_width;
                        ui.fonts(|f| f.layout_job(layout_job))
                    };
                let r = ui.add(
                    egui::TextEdit::multiline(&mut block.script)
                        .font(egui::TextStyle::Monospace) // for cursor height
                        .code_editor()
                        .desired_rows(10)
                        .lock_focus(true)
                        .desired_width(f32::INFINITY)
                        .layouter(&mut layouter),
                );
                if r.changed() {
                    out |= ViewResponse::CHANGED;
                }
                out
            })
            .inner;

        if let Some(block_data) = &mut block.data {
            if !block_data.stdout.is_empty() {
                ui.label("Output");
                ui.add(
                    egui::TextEdit::multiline(&mut block_data.stdout)
                        .interactive(false)
                        .desired_width(f32::INFINITY),
                );
            }
            if let Some(BlockError::EvalError(e)) = &block_data.error {
                ui.label("Errors");
                let mut text = e.message.clone();
                ui.scope(|ui| {
                    let vis = ui.visuals_mut();
                    vis.widgets.inactive = vis.widgets.active;
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .interactive(false)
                            .desired_width(f32::INFINITY),
                    );
                });
            }
        }
        out
    }
}

#[derive(Copy, Clone, Default)]
struct NameEdit {
    needs_focus: bool,
}

#[derive(Copy, Clone)]
pub struct BlockUiFlags {
    pub is_open: bool,
    pub is_last: bool,
    pub is_dragged: bool,
    pub is_view_open: Option<bool>,
}

/// Draws a draggable block within a [`egui_dnd`] context
///
/// Returns a [`BlockResponse`] based on button presses
pub fn draggable_block(
    ui: &mut egui::Ui,
    index: BlockIndex,
    block: &mut Block,
    flags: BlockUiFlags,
    handle: egui_dnd::Handle,
) -> BlockResponse {
    let mut response = BlockResponse::empty();
    let padding = ui.spacing().icon_width + ui.spacing().icon_spacing;
    if block.data.as_ref().is_some_and(|s| !s.io_values.is_empty()) {
        egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            index.id(),
            true,
        )
        .show_header(ui, |ui| {
            response = draggable_block_header(ui, index, block, flags, handle)
        })
        .body_unindented(|ui| {
            if block_body(ui, index, block) {
                response |= BlockResponse::CHANGED;
            }
            if !flags.is_last {
                ui.separator();
            }
        });
    } else {
        ui.horizontal(|ui| {
            ui.add_space(padding);
            response = draggable_block_header(ui, index, block, flags, handle)
        });
    }
    response
}

#[must_use]
fn block_body(ui: &mut egui::Ui, index: BlockIndex, block: &mut Block) -> bool {
    let mut changed = false;
    let block_data = block.data.as_ref().unwrap();
    let padding = ui.spacing().icon_width + ui.spacing().icon_spacing;
    for (name, value) in &block_data.io_values {
        ui.horizontal(|ui| {
            ui.add_space(padding);
            ui.label(name);
            match value {
                IoValue::Output(value) => {
                    let mut txt = value.to_string();
                    ui.add_enabled(
                        false,
                        egui::TextEdit::singleline(&mut txt)
                            .desired_width(f32::INFINITY),
                    );
                }
                IoValue::Input(value) => {
                    let s = block.inputs.get_mut(name).unwrap();
                    let input_id = index.id().with("input_edit").with(name);
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if let Err(err) = &value {
                                ui.colored_label(
                                    ui.style().visuals.error_fg_color,
                                    WARN,
                                )
                                .on_hover_ui(
                                    |ui| {
                                        ui.label(err);
                                    },
                                );
                            }
                            let r = ui.add(
                                egui::TextEdit::singleline(s)
                                    .id(input_id)
                                    .desired_width(f32::INFINITY),
                            );
                            if r.changed() {
                                changed = true;
                            }
                        },
                    );
                }
            }
        });
    }
    changed
}

fn draggable_block_header(
    ui: &mut egui::Ui,
    index: BlockIndex,
    block: &mut Block,
    flags: BlockUiFlags,
    handle: egui_dnd::Handle,
) -> BlockResponse {
    // Editable object name
    let mut response = BlockResponse::empty();
    // Buttons on the right side
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(5.0);
        handle.show_drag_cursor_on_hover(false).ui(ui, |ui| {
            ui.add(egui::Button::new(DRAG).selected(flags.is_dragged));
        });
        if ui.button(TRASH).clicked() {
            response |= BlockResponse::DELETE;
        }
        if ui
            .add(egui::Button::new(PENCIL).selected(flags.is_open))
            .clicked()
        {
            response = BlockResponse::TOGGLE_EDIT;
        }
        if let Some(view) = flags.is_view_open {
            if ui.add(egui::Button::new(EYE).selected(view)).clicked() {
                response = BlockResponse::TOGGLE_VIEW;
            }
        }
        if let Some(block_data) = &block.data {
            match &block_data.error {
                Some(BlockError::NameError(e)) => {
                    let err = match e {
                        NameError::DuplicateName => "duplicate name",
                        NameError::InvalidIdentifier => "invalid identifier",
                    };
                    ui.label(
                        egui::RichText::new(WARN)
                            .color(ui.style().visuals.error_fg_color),
                    )
                    .on_hover_ui(|ui| {
                        ui.label(err);
                    });
                }
                Some(BlockError::EvalError(_)) => {
                    let r = ui
                        .add(
                            egui::Label::new(
                                egui::RichText::new(WARN)
                                    .color(ui.style().visuals.error_fg_color),
                            )
                            .sense(egui::Sense::click()),
                        )
                        .on_hover_ui(|ui| {
                            ui.label("script contains error");
                        });
                    if r.clicked() {
                        response |= BlockResponse::FOCUS_ERR;
                    }
                }
                None => (),
            }
        }
        ui.with_layout(
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                if block_name(ui, index, block) {
                    response |= BlockResponse::CHANGED;
                }
            },
        );
    });
    response
}

/// Draws the name of a block, editable with a double-click
///
/// Returns `true` if the name has changed, `false` otherwise
fn block_name(ui: &mut egui::Ui, index: BlockIndex, block: &mut Block) -> bool {
    let id = index.id();
    let mut changed = false;
    match ui.memory(|mem| mem.data.get_temp(id)) {
        Some(NameEdit { needs_focus }) => {
            let response = ui.add(
                egui::TextEdit::singleline(&mut block.name)
                    .id(egui::Id::new(index).with("name_edit"))
                    .desired_width(f32::INFINITY),
            );
            let lost_focus = response.lost_focus();
            changed |= response.changed();
            ui.memory_mut(|mem| {
                if needs_focus {
                    mem.request_focus(response.id);
                    mem.data.insert_temp(id, NameEdit { needs_focus: false });
                }
                if lost_focus {
                    mem.data.remove_temp::<NameEdit>(id);
                }
            });
        }
        None => {
            let (enabled, name) = if block.name.is_empty() {
                (false, "[empty]")
            } else {
                (true, block.name.as_str())
            };
            let response = ui.add_enabled(
                enabled,
                egui::Label::new(name).sense(egui::Sense::click()),
            );
            if response.double_clicked() {
                ui.memory_mut(|mem| {
                    mem.data.insert_temp(id, NameEdit { needs_focus: true })
                });
            }
        }
    }
    changed
}

// Unicode symbols from Nerd Fonts, see https://www.nerdfonts.com/cheat-sheet
pub const NEW_BLOCK: &str = "\u{f067} New block";
const DRAG: &str = "\u{f0041}";
const TRASH: &str = "\u{f48e}";
const PENCIL: &str = "\u{f03eb}";
const WARN: &str = "\u{f071}";
const EYE: &str = "\u{f441}";
const CAMERA: &str = "\u{f03d}";
const HOURGLASS: &str = "\u{f252}";
const ERROR: &str = "\u{ea87}";
