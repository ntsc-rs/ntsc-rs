use eframe::egui::{self, DroppedFileHandle};

pub trait UiDndExt {
    fn show_dnd_overlay(
        &mut self,
        text: impl Into<egui::RichText>,
    ) -> Option<Vec<DroppedFileHandle>>;
}

impl UiDndExt for egui::Ui {
    fn show_dnd_overlay(
        &mut self,
        text: impl Into<egui::RichText>,
    ) -> Option<Vec<DroppedFileHandle>> {
        let max_rect = self.max_rect();

        let pointer_in_drop_area = self.ctx().input(|input| {
            input
                .pointer
                .latest_pos()
                .is_some_and(|p| max_rect.contains(p))
        });

        if pointer_in_drop_area {
            let files = self.ctx().take_dropped_files_last_frame();
            if files.is_some() {
                return files;
            }
        }

        let dragging_files = self
            .ctx()
            .input(|input| !input.raw.hovered_files.is_empty());
        if !dragging_files {
            return None;
        }

        let overlay_size = max_rect.size();
        let area = egui::Area::new(self.auto_id_with("dnd_overlay"))
            .fixed_pos(max_rect.left_top())
            .default_size(overlay_size);
        area.show(self.ctx(), |ui| {
            ui.allocate_ui(overlay_size, |ui| {
                egui::Frame::NONE
                    .fill(ui.visuals().extreme_bg_color.gamma_multiply(0.8))
                    .stroke(ui.style().visuals.widgets.noninteractive.bg_stroke)
                    .show(ui, |ui| {
                        ui.centered_and_justified(|ui| {
                            ui.heading(text);
                        });
                    });
            });
        });

        None
    }
}

pub trait CtxDndExt {
    fn update_dnd_state(&self);
    fn take_dropped_files_last_frame(&self) -> Option<Vec<DroppedFileHandle>>;
}

impl CtxDndExt for egui::Context {
    fn update_dnd_state(&self) {
        // Due to event order, dropped files may come in before the pointer position is updated. To avoid this, we need
        // to delay handling them by one frame.
        //
        // TODO: Remove this once winit 0.31 comes out and its DnD rework makes it into egui.
        let files_id = egui::Id::new("dropped_files_last_frame");

        let dropped_files = self.input_mut(|input| {
            if input.raw.dropped_files.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut input.raw.dropped_files))
            }
        });

        self.data_mut(|data| {
            data.remove_temp::<Vec<DroppedFileHandle>>(files_id);

            if let Some(dropped_files) = dropped_files {
                data.insert_temp(files_id, dropped_files);
            }
        });
    }

    fn take_dropped_files_last_frame(&self) -> Option<Vec<DroppedFileHandle>> {
        let files_id = egui::Id::new("dropped_files_last_frame");
        self.data_mut(|data| data.remove_temp::<Vec<DroppedFileHandle>>(files_id))
    }
}
