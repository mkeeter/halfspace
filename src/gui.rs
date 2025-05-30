//! Functions to draw our data into an `egui` context
use std::collections::HashMap;

use crate::{
    view::{
        RenderMode, RenderSettings, RenderSettings2D, ViewCanvas,
        ViewCanvasDiscriminants, ViewData,
    },
    world::{Block, BlockIndex, IoValue, NameError, World},
    BlockResponse, Message,
};

pub struct WorldView<'a> {
    pub world: &'a mut World,
    pub syntax: &'a egui_extras::syntax_highlighting::SyntectSettings,
    pub changed: &'a mut bool,
    pub views: &'a mut HashMap<BlockIndex, ViewData>,
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
        match tab.mode {
            TabMode::Script => self.script_ui(ui, tab.index),
            TabMode::View => self.view_ui(ui, tab.index),
        }
    }
}

impl<'a> WorldView<'a> {
    fn view_ui(&mut self, ui: &mut egui::Ui, index: BlockIndex) {
        let block = &self.world[index];
        let Some(block_view) = block.get_view() else {
            self.view_fallback_ui(ui, "block has error");
            return;
        };
        let rect = ui.clip_rect();
        let size = fidget::render::ImageSize::new(
            rect.width() as u32,
            rect.height() as u32,
        );
        let entry = self
            .views
            .entry(index)
            .or_insert_with(|| ViewData::new(size));
        let ctx = ui.ctx().clone();
        let view;
        let mode = match &entry.canvas {
            ViewCanvas::SdfApprox(c) => {
                view = c.view();
                RenderMode::SdfApprox(RenderSettings2D { view, size })
            }
            ViewCanvas::Bitfield(c) => {
                view = c.view();
                RenderMode::Bitfield(RenderSettings2D { view, size })
            }
        };
        let settings = RenderSettings {
            tree: block_view.tree.clone(),
            mode,
        };

        entry.check(index, settings, self.tx.clone(), move || {
            ctx.request_repaint()
        });

        let Some(image) = entry.image() else {
            self.view_fallback_ui(ui, "render in progress...");
            return;
        };
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            crate::draw::WgpuPainter::new(image.clone(), size, view),
        ));
        let r = ui.interact(
            rect,
            index.id().with("block_view_interact"),
            egui::Sense::click_and_drag(),
        );

        let cursor_state = match (r.interact_pointer_pos(), r.hover_pos()) {
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
        // Send mouse interactions to the canvas
        let mut render_changed = match &mut entry.canvas {
            ViewCanvas::SdfApprox(c) | ViewCanvas::Bitfield(c) => c.interact(
                size,
                cursor_state,
                ui.ctx().input(|i| i.smooth_scroll_delta.y),
            ),
        };

        // Pop-up box to change render settings
        let response = ui.button(CAMERA);
        let popup_id = ui.make_persistent_id(index.id().with("view_editor"));
        if response.clicked() {
            ui.memory_mut(|mem| mem.toggle_popup(popup_id));
        }
        let below = egui::AboveOrBelow::Below;
        let close_on_click_outside =
            egui::popup::PopupCloseBehavior::CloseOnClickOutside;
        let mut tag = ViewCanvasDiscriminants::from(&entry.canvas);
        egui::popup::popup_above_or_below_widget(
            ui,
            popup_id,
            &response,
            below,
            close_on_click_outside,
            |ui| {
                egui::ComboBox::from_label("View mode").show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut tag,
                        ViewCanvasDiscriminants::Bitfield,
                        "2D bitfield",
                    );
                    ui.selectable_value(
                        &mut tag,
                        ViewCanvasDiscriminants::SdfApprox,
                        "2D SDF (approx)",
                    );
                });
            },
        );
        match (tag, &entry.canvas) {
            (ViewCanvasDiscriminants::SdfApprox, ViewCanvas::Bitfield(c)) => {
                entry.canvas = ViewCanvas::SdfApprox(*c);
                render_changed = true;
            }
            (ViewCanvasDiscriminants::Bitfield, ViewCanvas::SdfApprox(c)) => {
                entry.canvas = ViewCanvas::Bitfield(*c);
                render_changed = true;
            }
            _ => (),
        }
        if render_changed {
            ui.ctx().request_repaint();
        }
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

    fn script_ui(&mut self, ui: &mut egui::Ui, index: BlockIndex) {
        let block = &mut self.world[index];
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
        *self.changed |= r.changed();
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
                                ui.label(
                                    egui::RichText::new(WARN).color(
                                        ui.style().visuals.error_fg_color,
                                    ),
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
