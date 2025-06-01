//! Functions to draw our data into an `egui` context
use std::collections::HashMap;

use crate::{
    view::{
        RenderMode, RenderSettings, RenderSettings2D, RenderSettings3D,
        ViewCanvas, ViewCanvasType, ViewData,
    },
    world::{Block, BlockIndex, IoValue, NameError, World},
    BlockResponse, Message, ViewResponse,
};

pub struct WorldView<'a> {
    pub world: &'a mut World,
    pub syntax: &'a egui_extras::syntax_highlighting::SyntectSettings,
    pub views: &'a mut HashMap<BlockIndex, ViewData>,
    pub out: &'a mut Vec<(BlockIndex, ViewResponse)>,
    pub tx: &'a std::sync::mpsc::Sender<Message>,
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
enum TabMode {
    Script,
    View,
}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
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

    /// Draw a block as as editable text pane
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
            self.view_fallback_ui(ui, "block has errors...");
            return out;
        }

        // At this point, either we have a block view, or we have a previous
        // view from this block.  We'll either get the previous entry or insert
        // a new empty one.
        let entry = entry.or_insert_with(|| ViewData::new(size));

        let ctx = ui.ctx().clone();
        // If we have a block view, then use it (or fall back to the previous
        // image, drawing it in a valid state).  Otherwise, fall back to the
        // previous image, drawing it in an *invalid* state (with a red border).
        let (image, valid) = if let Some(block_view) = block_view {
            let mode = match &entry.canvas {
                ViewCanvas::SdfApprox(c) => {
                    let view = c.view();
                    RenderMode::SdfApprox(RenderSettings2D { view, size })
                }
                ViewCanvas::SdfExact(c) => {
                    let view = c.view();
                    RenderMode::SdfExact(RenderSettings2D { view, size })
                }
                ViewCanvas::Bitfield(c) => {
                    let view = c.view();
                    RenderMode::Bitfield(RenderSettings2D { view, size })
                }
                ViewCanvas::Heightmap(c) => {
                    let view = c.view();
                    let size = fidget::render::VoxelSize::new(
                        size.width(),
                        size.height(),
                        size.width().max(size.height()),
                    );
                    RenderMode::Heightmap(RenderSettings3D { view, size })
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
                self.view_fallback_ui(ui, "render in progress...");
                return out;
            };
            (image, true)
        } else if let Some(prev_image) = entry.prev_image() {
            (prev_image, false)
        } else {
            // XXX can we actually get here?
            self.view_fallback_ui(ui, "no previous image");
            return out;
        };

        // This is the magic that triggers the GPU callback.  We pick a render
        // mode based on the selected image's settings
        match image.settings.mode {
            RenderMode::Bitfield(s)
            | RenderMode::SdfApprox(s)
            | RenderMode::SdfExact(s) => {
                ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                    rect,
                    crate::draw::WgpuBitmapPainter::new(
                        index,
                        image.clone(),
                        size,
                        s.view,
                    ),
                ));
            }
            RenderMode::Heightmap(s) => {
                todo!()
            }
        }

        let r = ui.interact(
            rect,
            index.id().with("block_view_interact"),
            egui::Sense::click_and_drag(),
        );

        // Send mouse interactions to the canvas
        let mut render_changed = match &mut entry.canvas {
            ViewCanvas::SdfApprox(c)
            | ViewCanvas::SdfExact(c)
            | ViewCanvas::Bitfield(c) => {
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
                c.interact(
                    size,
                    cursor_state,
                    if r.hover_pos().is_some() {
                        ui.ctx().input(|i| i.smooth_scroll_delta.y)
                    } else {
                        0.0
                    },
                )
            }
            ViewCanvas::Heightmap(c) => {
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

                c.interact(
                    size,
                    cursor_state,
                    ui.ctx().input(|i| i.smooth_scroll_delta.y),
                )
            }
        };

        if !valid {
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
                },
            );
        }

        // Pop-up box to change render settings
        let mut tag = ViewCanvasType::from(&entry.canvas);
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
                ui.selectable_value(
                    &mut tag,
                    ViewCanvasType::SdfApprox,
                    "2D SDF (approx)",
                );
                ui.selectable_value(
                    &mut tag,
                    ViewCanvasType::SdfExact,
                    "2D SDF (exact)",
                );
                ui.selectable_value(
                    &mut tag,
                    ViewCanvasType::Heightmap,
                    "3D heightmap",
                );
                ui.separator();
                if ui.button("Reset camera").clicked() {
                    reset_camera = true;
                }
            });
        // If we've edited the canvas tag, then update it in the entry
        if tag != (&entry.canvas).into() {
            render_changed = true;
            match (tag, &entry.canvas) {
                // If both the original and new canvas are 2D, then steal the
                // GUI canvas from the previous canvas to reuse it.
                (
                    ViewCanvasType::SdfExact
                    | ViewCanvasType::SdfApprox
                    | ViewCanvasType::Bitfield,
                    ViewCanvas::Bitfield(c)
                    | ViewCanvas::SdfExact(c)
                    | ViewCanvas::SdfApprox(c),
                ) => {
                    entry.canvas = match tag {
                        ViewCanvasType::SdfExact => ViewCanvas::SdfExact(*c),
                        ViewCanvasType::SdfApprox => ViewCanvas::SdfApprox(*c),
                        ViewCanvasType::Bitfield => ViewCanvas::Bitfield(*c),
                        ViewCanvasType::Heightmap => unreachable!(),
                    }
                }
                // we've gone from 2D to 3D (or vice versa), and therefore can't
                // reuse the canvas (TODO maybe reuse some of it?)
                (ViewCanvasType::SdfExact, _) => {
                    entry.canvas =
                        ViewCanvas::SdfExact(fidget::gui::Canvas2::new(size))
                }
                (ViewCanvasType::SdfApprox, _) => {
                    entry.canvas =
                        ViewCanvas::SdfApprox(fidget::gui::Canvas2::new(size))
                }
                (ViewCanvasType::Bitfield, _) => {
                    entry.canvas =
                        ViewCanvas::Bitfield(fidget::gui::Canvas2::new(size))
                }
                (ViewCanvasType::Heightmap, _) => {
                    // TODO control depth here?
                    let size = fidget::render::VoxelSize::new(
                        size.width(),
                        size.height(),
                        size.width().max(size.height()),
                    );
                    entry.canvas =
                        ViewCanvas::Heightmap(fidget::gui::Canvas3::new(size))
                }
            }
        }
        if reset_camera {
            match &mut entry.canvas {
                ViewCanvas::Bitfield(c)
                | ViewCanvas::SdfApprox(c)
                | ViewCanvas::SdfExact(c) => {
                    *c = fidget::gui::Canvas2::new(c.image_size());
                    render_changed = true;
                }
                ViewCanvas::Heightmap(c) => {
                    *c = fidget::gui::Canvas3::new(c.image_size());
                    render_changed = true;
                }
            }
        }
        if render_changed {
            ui.ctx().request_repaint();
        }
        out
    }

    /// Manually draw a backdrop indicating that the view is invalid
    fn view_fallback_ui(&mut self, ui: &mut egui::Ui, txt: &str) {
        let style = ui.style();
        let painter = ui.painter();
        let layout = painter.layout(
            txt.to_owned(),
            style.text_styles[&egui::TextStyle::Heading].clone(),
            style.visuals.widgets.noninteractive.text_color(),
            f32::INFINITY,
        );
        let rect = painter.clip_rect();
        let text_corner = rect.center() - layout.size() / 2.0;
        painter.rect_filled(rect, 0.0, style.visuals.panel_fill);
        painter.galley(text_corner, layout, egui::Color32::BLACK);
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
        let mut layouter = |ui: &egui::Ui, buf: &str, wrap_width: f32| {
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
        if let Some(state) = &mut block.state {
            if !state.stdout.is_empty() {
                ui.label("Output");
                ui.add(
                    egui::TextEdit::multiline(&mut state.stdout)
                        .interactive(false)
                        .desired_width(f32::INFINITY),
                );
            }
            if !state.script_errors.is_empty() {
                ui.label("Errors");
                let mut text = state
                    .script_errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
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
    if block
        .state
        .as_ref()
        .is_some_and(|s| !s.io_values.is_empty())
    {
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
    let state = block.state.as_ref().unwrap();
    let padding = ui.spacing().icon_width + ui.spacing().icon_spacing;
    for (name, value) in &state.io_values {
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
                    let input_id = index.id().with("input_edit");
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
        if let Some(state) = &block.state {
            if let Some(e) = state.name_error {
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
            } else if !state.script_errors.is_empty() {
                let r = ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(WARN)
                                .color(ui.style().visuals.warn_fg_color),
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
