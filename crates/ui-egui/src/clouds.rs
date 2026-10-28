//! Point clouds: Insert > Point Cloud asks for scan files, then a box for how to bring them in (name, units, spacing,
//! positioning, as Revit's Link Point Cloud asks), and runs `point_cloud`. A linked scan's cache is built on a thread of its
//! own first (`cloud::cached`), since a large file takes a while, and the command run when it's done finds it at once;
//! Reload Point Cloud, on the right-click menu, does the same for `reload_cloud`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use bimcraft_core::cloud::{self, cache::Progress};
use bimcraft_core::model::Element;
use egui::{Align, Button, Context, Grid, Id as UiId, Layout, Modal, ProgressBar, RichText, TextEdit};
use serde_json::{Value, json};

use crate::BimApp;
use crate::lang::{t, tf};
use crate::properties::number;
use crate::style::pal;

/// The units a scan's coordinates may be in, as `point_cloud` names them.
const UNITS: [&str; 5] = ["m", "mm", "cm", "in", "ft"];

/// Bringing scan files in: what was picked and how, until OK or Cancel.
pub(crate) struct CloudBox {
    paths: Vec<PathBuf>,
    name: String,
    units: &'static str,
    /// One point kept in each cube this wide, mm, as typed; blank keeps them all.
    spacing: String,
    /// Where its file's coordinates put it (Origin to Origin), or its middle on the origin (Center to Center).
    by_file: bool,
    /// Every point kept in the user's cache and the project only the upper levels, as `point_cloud`'s `link`.
    link: bool,
    /// Why it couldn't be brought in, shown in the box.
    error: Option<String>,
}

/// A linked scan's cache being built, and what to run when it's done.
pub(crate) struct CloudJob {
    done: Receiver<Result<(), String>>,
    /// How far it has got, for the gauge.
    progress: Arc<Progress>,
    then: Then,
}

enum Then {
    /// `point_cloud`, with these parameters, from the box.
    Insert(Value),
    /// `reload_cloud` of this scan.
    Reload(String),
}

/// Builds the cache of the file at `path` on a thread of its own, and then runs `then`.
fn build(path: PathBuf, format: cloud::Format, scale: f64, then: Then) -> CloudJob {
    let (tx, rx) = std::sync::mpsc::channel();
    let progress = Arc::new(Progress::default());
    let building = Arc::clone(&progress);
    std::thread::spawn(move || {
        let _ = tx.send(cloud::cached(&path, format, scale, &building).map(|_| ()));
    });
    CloudJob { done: rx, progress, then }
}

impl BimApp {
    /// Insert > Point Cloud: the files the user picks, then the box for them.
    pub(crate) fn point_cloud_dialog(&mut self, ctx: &Context) {
        let formats = ["pts", "xyz", "txt", "ply", "las", "laz", "e57", "rcp", "rcs"];
        let dialog = rfd::AsyncFileDialog::new().add_filter(t("Point clouds"), &formats);
        self.ask_files(ctx, dialog, |app, paths| app.point_cloud_paths(paths));
    }

    /// Opens the box for bringing in the scan files `paths`, named for the first.
    pub(crate) fn point_cloud_paths(&mut self, paths: Vec<PathBuf>) {
        let name = paths.first().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        // A large file is linked unless the user says otherwise, as `point_cloud` does.
        let link = matches!(paths.as_slice(), [one] if std::fs::metadata(one).is_ok_and(|m| m.len() > cloud::LINK_ABOVE));
        self.cloud_box = Some(CloudBox { paths, name, units: "m", spacing: String::new(), by_file: false, link, error: None });
    }

    pub(crate) fn point_cloud_window(&mut self, ctx: &Context) {
        self.poll_cloud_job(ctx);
        let gauge = self.cloud_job.as_ref().filter(|j| matches!(j.then, Then::Insert(_))).map(|j| j.progress.fraction());
        let building = gauge.is_some();
        let Some(b) = self.cloud_box.as_mut() else { return };
        let (mut ok, mut close) = (false, false);
        let r = Modal::new(UiId::new("point-cloud")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(RichText::new(t("Point Cloud")).heading());
            ui.add_space(6.0);
            let what = match b.paths.as_slice() {
                [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                all => tf("{} files, brought in as one scan's stations", &[&all.len().to_string()]),
            };
            ui.label(what);
            ui.add_space(6.0);
            Grid::new("point-cloud-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                let l = ui.label(t("Name"));
                ui.add_sized([220.0, 20.0], TextEdit::singleline(&mut b.name)).labelled_by(l.id);
                ui.end_row();
                let l = ui.label(t("Units"));
                let c = egui::ComboBox::from_id_salt("point-cloud-units").selected_text(b.units).show_ui(ui, |ui| {
                    for u in UNITS {
                        ui.selectable_value(&mut b.units, u, u);
                    }
                });
                c.response.labelled_by(l.id);
                ui.end_row();
                let l = ui.label(t("Spacing (mm)"));
                ui.add_enabled_ui(!b.link, |ui| {
                    ui.add_sized([220.0, 20.0], TextEdit::singleline(&mut b.spacing).hint_text(t("all the points"))).labelled_by(l.id);
                });
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.add_enabled_ui(b.paths.len() == 1, |ui| {
                ui.checkbox(&mut b.link, t("Link the file")).on_hover_text(t(
                    "Every point of it kept in this computer's cache, built once, and the project only a preview: a scan of any size, drawn in its detail",
                ));
            });
            ui.add_space(6.0);
            ui.label(t("Positioning"));
            ui.radio_value(&mut b.by_file, false, t("Auto - Center to Center"))
                .on_hover_text(t("Its middle on the project's origin and its foot on the level"));
            ui.radio_value(&mut b.by_file, true, t("Auto - Origin to Origin"))
                .on_hover_text(t("Where its file's coordinates put it, as the survey placed it"));
            if let Some(e) = &b.error {
                ui.add_space(6.0);
                ui.label(RichText::new(e).color(pal().error));
            }
            if let Some(f) = gauge {
                ui.add_space(6.0);
                ui.label(t("Building the scan's cache…"));
                ui.add(ProgressBar::new(f).show_percentage());
            }
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                close = ui.add_enabled(!building, Button::new(t("Cancel"))).clicked();
                ok = ui.add_enabled(!building && !b.name.trim().is_empty(), Button::new(t("OK"))).clicked();
            });
        });
        if ok {
            self.bring_in_cloud();
        } else if !building && (close || r.should_close()) {
            self.cloud_box = None;
        }
    }

    /// Runs what waited for a linked scan's cache, once it's built; until then, looks again soon.
    fn poll_cloud_job(&mut self, ctx: &Context) {
        let Some(job) = &self.cloud_job else { return };
        let result = match job.done.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => {
                if matches!(job.then, Then::Reload(_)) {
                    let done = (job.progress.fraction() * 100.0).floor();
                    self.status = crate::Status { text: format!("{} {done}%", t("Building the scan's cache…")), error: false };
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(TryRecvError::Disconnected) => Err("the scan's cache couldn't be built".into()),
        };
        let Some(job) = self.cloud_job.take() else { return };
        match (job.then, result) {
            (Then::Insert(p), Ok(())) => self.insert_cloud(p),
            (Then::Insert(_), Err(e)) => {
                if let Some(b) = self.cloud_box.as_mut() {
                    b.error = Some(e);
                }
            }
            (Then::Reload(name), Ok(())) => {
                self.run("reload_cloud", json!({"cloud": name}));
            }
            (Then::Reload(name), Err(e)) => self.status = crate::Status { text: format!("{name}: {e}"), error: true },
        }
    }

    /// Whether a linked scan's cache is being built.
    pub(crate) fn building_cloud(&self) -> bool {
        self.cloud_job.is_some()
    }

    /// A build for the box's scan that a test moves on itself: how far it has got, and the end of it to send.
    #[cfg(test)]
    pub(crate) fn hold_cloud_build(&mut self) -> (std::sync::mpsc::Sender<Result<(), String>>, Arc<Progress>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let progress = Arc::new(Progress::default());
        self.cloud_job = Some(CloudJob { done: rx, progress: Arc::clone(&progress), then: Then::Insert(json!({})) });
        (tx, progress)
    }

    /// The linked scans the selection places, by name.
    pub(crate) fn selected_linked_clouds(&self) -> Vec<String> {
        let m = &self.session.model;
        let mut names: Vec<String> = self
            .selection
            .iter()
            .filter_map(|id| match m.elements.get(*id) {
                Some(Element::PointCloud(pc)) if m.clouds.get(&pc.cloud).is_some_and(|c| c.link().is_some()) => Some(pc.cloud.clone()),
                _ => None,
            })
            .collect();
        names.dedup();
        names
    }

    /// Reload Point Cloud: the first linked scan the selection places, read again from its file, its cache built on a thread
    /// first.
    pub(crate) fn reload_cloud(&mut self) {
        let Some(name) = self.selected_linked_clouds().into_iter().next() else { return };
        let Some(link) = self.session.model.clouds.get(&name).and_then(|c| c.link()) else { return };
        let path = PathBuf::from(link.path());
        let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        match cloud::Format::from_extension(&ext) {
            Ok(format) => {
                self.cloud_job = Some(build(path, format, link.scale(), Then::Reload(name)));
                self.status = crate::Status { text: t("Building the scan's cache…").to_string(), error: false };
            }
            Err(_) => {
                self.run("reload_cloud", json!({"cloud": name}));
            }
        }
    }

    /// Runs `point_cloud` for the box's files: placed on the plan on screen's level, and selected. A refusal keeps the box
    /// open with the reason in it.
    fn bring_in_cloud(&mut self) {
        let Some(b) = self.cloud_box.as_ref() else { return };
        let mut p = json!({
            "path": b.paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>(),
            "name": b.name.trim(),
            "units": b.units,
            "origin": if b.by_file { "file" } else { "centre" },
            "link": b.link,
        });
        if !b.spacing.trim().is_empty() {
            match number(&b.spacing) {
                Some(s) => p["spacing"] = json!(s),
                None => {
                    let e = tf("`{}` is not a number", &[b.spacing.trim()]);
                    if let Some(b) = self.cloud_box.as_mut() {
                        b.error = Some(e);
                    }
                    return;
                }
            }
        }
        if let crate::Sheet::Plan(key) | crate::Sheet::Ceiling(key) = self.sheet
            && let Some(level) = self.session.model.plan_level(key)
        {
            p["level"] = json!(level);
        }
        // A linked file's cache is built first, away from the window; a file it can't read says why when the command runs.
        if let (true, [path]) = (b.link, b.paths.as_slice()) {
            let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
            if let (Ok(format), Some(scale)) = (cloud::Format::from_extension(&ext), cloud::unit(b.units)) {
                self.cloud_job = Some(build(path.clone(), format, scale, Then::Insert(p)));
                if let Some(b) = self.cloud_box.as_mut() {
                    b.error = None;
                }
                return;
            }
        }
        self.insert_cloud(p);
    }

    /// Runs `point_cloud` with the box's parameters `p`.
    fn insert_cloud(&mut self, p: Value) {
        match self.run("point_cloud", p) {
            Some(v) => {
                self.cloud_box = None;
                self.selection = v["id"].as_u64().into_iter().collect();
            }
            // Said in the box, not on the status bar under it.
            None => {
                let e = std::mem::take(&mut self.status).text;
                if let Some(b) = self.cloud_box.as_mut() {
                    b.error = Some(e);
                }
            }
        }
    }
}
