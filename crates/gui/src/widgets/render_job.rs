use std::borrow::Cow;

use eframe::egui::{self, InnerResponse, Response};
use gstreamer::ClockTime;

use crate::app::{
    error::ApplicationError,
    format_eta::format_eta,
    render_job::{RenderJob, RenderJobProgress, RenderJobState},
};

pub struct RenderJobWidget<'a> {
    render_job: &'a mut RenderJob,
}

impl<'a> RenderJobWidget<'a> {
    pub fn new(render_job: &'a mut RenderJob) -> Self {
        Self { render_job }
    }
}

pub struct RenderJobResponse {
    pub response: Response,
    pub closed: bool,
    pub error: Option<ApplicationError>,
}

impl RenderJobWidget<'_> {
    pub fn show(self, ui: &mut egui::Ui) -> RenderJobResponse {
        let job = self.render_job;
        let InnerResponse {
            inner: (closed, error),
            response,
        } = ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
            let mut closed = false;
            let mut error = None;

            let fill = ui.style().visuals.faint_bg_color;
            egui::Frame::NONE
                .fill(fill)
                .stroke(ui.style().visuals.window_stroke)
                .corner_radius(ui.style().noninteractive().corner_radius)
                .inner_margin(ui.style().spacing.window_margin)
                .show(ui, |ui| {
                    let job_state = job.state.lock().unwrap().clone();

                    let RenderJobProgress {
                        progress,
                        position: job_position,
                        duration: job_duration,
                        estimated_time_remaining,
                    } = job.update_progress(ui.ctx());

                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            closed = ui.button("🗙").clicked();

                            match &job_state {
                                RenderJobState::Rendering => {
                                    if ui.button("⏸").clicked() {
                                        let current_time = ui.ctx().input(|input| input.time);
                                        error = job.pause_at_time(current_time).err();
                                    }
                                }
                                RenderJobState::Paused if ui.button("▶").clicked() => {
                                    let current_time = ui.ctx().input(|input| input.time);
                                    error = job.resume_at_time(current_time).err();
                                }
                                _ => {}
                            }

                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.add(
                                    egui::Label::new(job.settings.output_path.to_string_lossy())
                                        .truncate(),
                                );
                            })
                        });
                    });

                    ui.separator();

                    ui.add(egui::ProgressBar::new(progress as f32).show_percentage());
                    if let RenderJobState::Rendering = job_state {
                        ui.ctx().request_repaint();
                    }

                    ui.label(match &job_state {
                        RenderJobState::Waiting => Cow::Borrowed("Waiting..."),
                        RenderJobState::Rendering => {
                            if let (Some(position), Some(duration)) = (job_position, job_duration) {
                                Cow::Owned(format!(
                                    "Rendering... ({:.2} / {:.2})",
                                    position, duration
                                ))
                            } else {
                                Cow::Borrowed("Rendering...")
                            }
                        }
                        RenderJobState::Paused => Cow::Borrowed("Paused"),
                        // if the job's start_time is missing, it's probably because it never got a chance to update--in that case, just say it took 0 seconds
                        RenderJobState::Complete { end_time } => Cow::Owned(format!(
                            "Completed in {:.2}",
                            ClockTime::from_mseconds(
                                ((*end_time - job.start_time.unwrap_or(*end_time)) * 1000.0) as u64
                            )
                        )),
                        RenderJobState::Error(err) => Cow::Owned(format!("Error: {err}")),
                    });

                    if matches!(
                        job_state,
                        RenderJobState::Rendering | RenderJobState::Paused
                    ) && let Some(time_remaining) = estimated_time_remaining
                    {
                        let mut label = String::from("Time remaining: ");
                        format_eta(
                            &mut label,
                            time_remaining,
                            [
                                [" hour", " hours"],
                                [" minute", " minutes"],
                                [" second", " seconds"],
                            ],
                            ", ",
                        );
                        ui.label(&label);
                    }
                    if matches!(job_state, RenderJobState::Complete { .. }) {
                        ui.horizontal(|ui| {
                            if ui.button("Open containing folder").clicked()
                                && let Some(parent) = job.settings.output_path.parent()
                            {
                                let _ = open::that_detached(parent);
                            }
                        });
                    }
                });

            (closed, error)
        });

        RenderJobResponse {
            response,
            closed,
            error,
        }
    }
}
