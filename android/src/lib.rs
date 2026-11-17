use eframe::egui;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    let options = eframe::NativeOptions {
        android_app: Some(app),
        ..Default::default()
    };

    if let Err(error) = eframe::run_native(
        "Furcadia Native",
        options,
        Box::new(|_creation_context| Ok(Box::new(AndroidPortApp))),
    ) {
        log::error!("Android client exited: {error}");
    }
}

struct AndroidPortApp;

impl eframe::App for AndroidPortApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("header").show(context, |ui| {
            ui.heading("Furcadia Native");
            ui.label("Android port");
        });

        egui::CentralPanel::default().show(context, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(32.0);
                ui.heading("Port foundation");
                ui.add_space(12.0);
                ui.label("This Android app shell is ready for the mobile client interface.");
                ui.label(
                    "Next: connect the shared Furcadia client core and build the sign-in flow.",
                );
            });
        });
    }
}
