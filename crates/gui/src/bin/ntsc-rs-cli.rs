use std::{
    fmt::Write as FmtWrite,
    fs,
    io::{self, Write},
    ops::RangeInclusive,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use clap::{
    Arg, ArgAction, ArgGroup, ValueEnum,
    builder::{EnumValueParser, PathBufValueParser, PossibleValue},
    command,
};
use color_eyre::eyre::{Report, Result, WrapErr};
use console::{StyledObject, Term, measure_text_width, style, truncate_str};
use gstreamer::ClockTime;
use ntsc_rs::{
    NtscEffect,
    settings::{ParseSettingsError, SettingsList},
};
use ntsc_rs_gui::{
    app::{
        self,
        executor::ApplessExecutor,
        format_eta::format_eta,
        render_job::{RenderJob, RenderJobState, SharedRenderJob},
        render_settings::{
            Ffv1BitDepth, Ffv1Settings, H264Settings, OutputCodec, PngSequenceSettings,
            PngSettings, RenderPipelineCodec, RenderPipelineSettings, StillImageSettings,
        },
        ui_context::UIContext,
    },
    gst_utils::{
        clock_format::clock_time_parser,
        init::initialize_gstreamer,
        ntsc_pipeline::{VideoScale, VideoScaleFilter},
    },
};

fn parse_settings(
    settings_list: &SettingsList<NtscEffect>,
    json: &str,
) -> Result<NtscEffect, ParseSettingsError> {
    settings_list.from_json(json)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputCodecArg {
    H264,
    Ffv1,
    Png,
}

impl OutputCodecArg {
    fn extension(&self) -> &'static str {
        match self {
            Self::H264 => OutputCodec::H264.extension(),
            Self::Ffv1 => OutputCodec::Ffv1.extension(),
            Self::Png => "png",
        }
    }
}

impl ValueEnum for OutputCodecArg {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::H264, Self::Ffv1, Self::Png]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(match self {
            Self::H264 => PossibleValue::new("h264"),
            Self::Ffv1 => PossibleValue::new("ffv1"),
            Self::Png => PossibleValue::new("png"),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct VideoScaleFilterArg(VideoScaleFilter);

impl ValueEnum for VideoScaleFilterArg {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self(VideoScaleFilter::Nearest),
            Self(VideoScaleFilter::Bilinear),
            Self(VideoScaleFilter::Bicubic),
        ]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(
            match self.0 {
                VideoScaleFilter::Nearest => PossibleValue::new("nearest"),
                VideoScaleFilter::Bilinear => PossibleValue::new("bilinear"),
                VideoScaleFilter::Bicubic => PossibleValue::new("bicubic"),
            }
            .help(self.0.label_and_tooltip().1),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Ffv1BitDepthArg(Ffv1BitDepth);

impl ValueEnum for Ffv1BitDepthArg {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self(Ffv1BitDepth::Bits8),
            Self(Ffv1BitDepth::Bits10),
            Self(Ffv1BitDepth::Bits12),
        ]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(match self.0 {
            Ffv1BitDepth::Bits8 => PossibleValue::new("8"),
            Ffv1BitDepth::Bits10 => PossibleValue::new("10"),
            Ffv1BitDepth::Bits12 => PossibleValue::new("12"),
        })
    }
}

fn clock_time_parser_clap(input: &str) -> Result<gstreamer::ClockTime> {
    match clock_time_parser(input) {
        Some(ms) => Ok(gstreamer::ClockTime::from_mseconds(ms as u64)),
        None => Err(Report::msg(format!("Not a valid time: {}", input))),
    }
}

fn as_i64_range<T: Into<i64> + Copy>(range: RangeInclusive<T>) -> RangeInclusive<i64> {
    (*range.start()).into()..=(*range.end()).into()
}

macro_rules! warn {
    ($dst:expr, $($arg:tt)*) => {
        writeln!(
            $dst,
            "{}",
            style(format!($($arg)*)).yellow()
        )
    }
}

pub fn main() -> Result<()> {
    color_eyre::install()?;
    let settings_list = SettingsList::<NtscEffect>::new();
    let settings_list_for_parser = settings_list.clone();

    let parse_standard_settings = move |json: &str| parse_settings(&settings_list_for_parser, json);

    let command = command!()
        .name("ntsc-rs")
        .arg(
            Arg::new("input")
                .short('i')
                .long("input")
                .value_parser(PathBufValueParser::new())
                .help("Path to the input media.")
                .required(true),
        )
        .arg(
            Arg::new("output")
                .short('o')
                .long("output")
                .value_parser(PathBufValueParser::new())
                .help("Name/path of the file(s) to render to.")
                .long_help(
                    "Name/path of the file(s) to render to. If the codec is an image, sequential \
                     \"#\" characters in the output filename will be replaced with the frame \
                     number.",
                )
                .required(true),
        )
        .arg(
            Arg::new("overwrite")
                .short('y')
                .long("overwrite")
                .action(ArgAction::SetTrue)
                .help(
                    "If the output file already exists, overwrite it without first prompting the \
                     user.",
                )
                .conflicts_with("no-overwrite"),
        )
        .arg(
            Arg::new("no-overwrite")
                .short('n')
                .long("no-overwrite")
                .action(ArgAction::SetTrue)
                .help(
                    "If the output file already exists, exit immediately without first prompting \
                     the user.",
                )
                .conflicts_with("overwrite"),
        )
        .arg(
            Arg::new("settings-path")
                .short('p')
                .long("settings-path")
                .value_parser(PathBufValueParser::new())
                .help("Path to a JSON effect settings preset.")
                .conflicts_with("settings-json"),
        )
        .arg(
            Arg::new("settings-json")
                .short('j')
                .long("settings-json")
                // TODO: ValueParser that wraps ntsc_rs::settings
                .help("JSON string for an effect settings preset.")
                .conflicts_with("settings-path")
                .value_parser(parse_standard_settings.clone()),
        )
        .group(ArgGroup::new("settings").args(["settings-path", "settings-json"]))
        .arg(
            Arg::new("fps")
                .long("fps")
                .help("Framerate to use if the input is a still image.")
                .value_parser(clap::value_parser!(u32))
                .default_value("30"),
        )
        .arg(
            Arg::new("duration")
                .long("duration")
                .help("Duration to use if the input is a still image.")
                .value_parser(clock_time_parser_clap)
                .default_value("00:05.00"),
        )
        .arg(
            Arg::new("scale")
                .long("scale")
                .help("Height (in lines) to resize the input media to before applying the effect.")
                .value_parser(clap::value_parser!(u32)),
        )
        .arg(
            Arg::new("scale-filter")
                .long("scale-filter")
                .help("Filter to use if resizing the input media.")
                .value_parser(EnumValueParser::<VideoScaleFilterArg>::new())
                .default_value("bilinear"),
        )
        .arg(
            Arg::new("interlace")
                .long("interlace")
                .help(
                    "Interlace progressive (non-interlaced) input media. Has no effect on media \
                     that's already interlaced.",
                )
                .action(ArgAction::SetTrue),
        )
        .next_help_heading("Codec settings")
        .arg(
            Arg::new("codec")
                .short('c')
                .long("codec")
                .help("Which video codec to encode the output with.")
                .value_parser(EnumValueParser::<OutputCodecArg>::new())
                .default_value("h264"),
        )
        .arg(
            Arg::new("chroma-subsampling")
                .long("chroma-subsampling")
                .help(
                    "Encode the chrominance (color) information at a lower resolution than the \
                     luminance (brightness). Maximizes playback compatibility for H.264 videos \
                     when enabled. Also works with FFV1.",
                )
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("quality")
                .long("quality")
                .help(
                    "Video quality factor for H.264 encoding. Ranges from 0-50, where 50 is the \
                     best quality and 0 is the worst.",
                )
                .value_parser(
                    clap::value_parser!(u8).range(as_i64_range(H264Settings::QUALITY_RANGE)),
                )
                .default_value(H264Settings::default().quality.to_string()),
        )
        .arg(
            Arg::new("encoding-speed")
                .long("encoding-speed")
                .help(
                    "Encoding speed for H.264 encoding. Ranges from 0-8, where 8 is fastest and 0 \
                     is smallest.",
                )
                .value_parser(
                    clap::value_parser!(u8).range(as_i64_range(H264Settings::ENCODE_SPEED_RANGE)),
                )
                .default_value(H264Settings::default().encode_speed.to_string()),
        )
        .arg(
            Arg::new("bit-depth")
                .long("bit-depth")
                .help("Bit depth for FFV1 encoding.")
                .value_parser(EnumValueParser::<Ffv1BitDepthArg>::new())
                .default_value(
                    Ffv1BitDepthArg::to_possible_value(&Ffv1BitDepthArg::default())
                        .unwrap()
                        .get_name()
                        .to_owned(),
                ),
        )
        .arg(
            Arg::new("compression-level")
                .long("compression-level")
                .help(
                    "Compression level for PNG encoding. Ranges from 0-9, where 0 is fastest and \
                     9 is smallest.",
                )
                .value_parser(
                    clap::value_parser!(u8)
                        .range(as_i64_range(PngSequenceSettings::COMPRESSION_LEVEL_RANGE)),
                )
                .default_value(PngSequenceSettings::default().compression_level.to_string()),
        )
        .arg(
            Arg::new("single-frame-time")
                .long("single-frame-time")
                .help(
                    "If provided and the \"png\" codec is used, a single frame at this time will \
                     be rendered.",
                )
                .value_parser(clock_time_parser_clap),
        );

    let matches = command.get_matches();

    let settings = if let Some(settings_path) = matches.get_one::<PathBuf>("settings-path") {
        parse_standard_settings(
            std::str::from_utf8(&fs::read(settings_path).wrap_err("Failed to open settings file")?)
                .wrap_err("Settings file is not valid UTF-8")?,
        )
        .wrap_err("Failed to parse settings file")?
    } else if let Some(settings) = matches.get_one::<NtscEffect>("settings-json") {
        settings.clone()
    } else {
        Default::default()
    };

    let input_path = matches
        .get_one::<PathBuf>("input")
        .expect("input path is present");
    let mut output_path = matches
        .get_one::<PathBuf>("output")
        .expect("output path is present")
        .to_owned();
    let overwrite = matches.get_flag("overwrite");
    let no_overwrite = matches.get_flag("no-overwrite");
    let framerate = matches
        .get_one::<u32>("fps")
        .expect("framerate is present")
        .to_owned();
    let duration = matches
        .get_one::<gstreamer::ClockTime>("duration")
        .expect("duration is present")
        .to_owned();
    let filter = matches
        .get_one::<VideoScaleFilterArg>("scale-filter")
        .expect("scale-filter is present")
        .0;
    let scale = matches.get_one::<u32>("scale").map(|scale| VideoScale {
        scanlines: *scale as usize,
        filter,
    });
    let interlace = matches.get_flag("interlace");
    let codec = matches
        .get_one::<OutputCodecArg>("codec")
        .expect("codec is present");
    let chroma_subsampling = matches.get_flag("chroma-subsampling");
    let quality = matches
        .get_one::<u8>("quality")
        .expect("quality is present")
        .to_owned();
    let encode_speed = matches
        .get_one::<u8>("encoding-speed")
        .expect("encoding speed is present")
        .to_owned();
    let bit_depth = matches
        .get_one::<Ffv1BitDepthArg>("bit-depth")
        .expect("bit depth is present")
        .0;
    let compression_level = matches
        .get_one::<u8>("compression-level")
        .expect("compression level is present")
        .to_owned();
    let single_frame_time = matches
        .get_one::<gstreamer::ClockTime>("single-frame-time")
        .copied();

    let mut term = Term::buffered_stdout();

    const VERSION: &str = env!("CARGO_PKG_VERSION");
    writeln!(
        term,
        "{}{}{}{}{}{}{} v{VERSION}",
        style("n"),
        style("t").yellow(),
        style("s").cyan(),
        style("c").green(),
        style("-").magenta(),
        style("r").red(),
        style("s").blue()
    )?;
    term.flush()?;

    if output_path.extension().is_none() {
        output_path.set_extension(codec.extension());
    }

    if let Some(extension) = output_path.extension()
        && extension != codec.extension()
    {
        warn!(
            term,
            "Warning: the provided output file name's extension (.{}) does not match the file \
                 extension for the container being used (.{})",
            extension.to_string_lossy(),
            codec.extension(),
        )?;
    }

    if single_frame_time.is_some() && *codec != OutputCodecArg::Png {
        warn!(
            term,
            "Warning: the --single-frame-time argument only has an effect if the codec is \"png\".",
        )?;
    }

    let output_path_metadata = fs::metadata(&output_path);
    if output_path_metadata
        .as_ref()
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(Report::msg(format!(
            "Output path {} is a folder",
            output_path.as_os_str().to_string_lossy()
        )));
    }

    if let (Ok(_), false) = (output_path_metadata, overwrite) {
        let should_exit = if term.is_term() && !no_overwrite {
            loop {
                write!(
                    term,
                    "{} already exists. Overwrite? [y/N] ",
                    output_path.as_os_str().to_string_lossy()
                )?;
                term.flush()?;
                let mut response = String::new();
                io::stdin().read_line(&mut response)?;
                response.make_ascii_lowercase();
                let response = response.trim();
                if response == "y" || response == "yes" {
                    break false;
                } else if response == "n" || response == "no" {
                    break true;
                }
            }
        } else {
            true
        };
        if should_exit {
            term.write_line("Not overwriting existing file. Exiting.")?;
            term.flush()?;
            return Ok(());
        }
    }

    initialize_gstreamer()?;

    let executor = CliExecutor(Arc::new(async_executor::Executor::new()));

    let (output, handle) = CliOutput::new(term.clone());

    let render_job = RenderJob::create(
        &executor,
        &output.clone(),
        input_path,
        RenderPipelineSettings {
            codec_settings: match (codec, single_frame_time) {
                (OutputCodecArg::H264, _) => RenderPipelineCodec::H264(H264Settings {
                    quality,
                    encode_speed,
                    ten_bit: false,
                    chroma_subsampling,
                }),
                (OutputCodecArg::Ffv1, _) => RenderPipelineCodec::Ffv1(Ffv1Settings {
                    bit_depth,
                    chroma_subsampling,
                }),
                (OutputCodecArg::Png, None) => {
                    RenderPipelineCodec::PngSequence(PngSequenceSettings { compression_level })
                }
                (OutputCodecArg::Png, Some(seek_to)) => RenderPipelineCodec::Png(PngSettings {
                    seek_to,
                    settings: PngSequenceSettings { compression_level },
                }),
            },
            output_path: output_path.clone(),
            interlaced_output: settings.use_field.interlaced_output_allowed() && interlace,
            effect_settings: settings,
        },
        &StillImageSettings {
            framerate: gstreamer::Fraction::from_integer(framerate as i32),
            duration,
        },
        scale,
    )?;
    let render_job = SharedRenderJob::new(render_job);

    output.set_render_job(Some(render_job.clone()));

    let job_state = futures_lite::future::block_on(executor.0.run(render_job));
    output.close()?;
    handle.join().unwrap()?;

    match job_state {
        RenderJobState::Waiting | RenderJobState::Rendering | RenderJobState::Paused => {
            return Err(Report::msg(
                "Render job still in progress when it should be finished",
            ));
        }
        RenderJobState::Complete { end_time } => {
            writeln!(
                term,
                "Finished rendering in {end_time:.0} second(s) to {}",
                output_path.as_os_str().to_string_lossy()
            )?;
            term.flush()?;
        }
        RenderJobState::Error(err) => {
            return Err(err.into());
        }
    }

    Ok(())
}

#[derive(Clone, Debug)]
struct CliExecutor(Arc<async_executor::Executor<'static>>);

impl ApplessExecutor for CliExecutor {
    fn spawn(
        &self,
        future: impl std::future::Future<Output = Option<app::ApplessFn>> + 'static + Send,
    ) {
        let inner = &self.0;
        inner
            .spawn(async {
                // TODO: display these errors
                if let Some(cb) = future.await {
                    cb()
                } else {
                    Ok(())
                }
            })
            .detach();
    }
}

#[derive(Debug, Clone)]
struct CliOutputInner {
    sender: Sender<bool>,
    render_job: Option<SharedRenderJob>,
    start_time: Instant,
    term: Term,
}

impl CliOutputInner {
    fn close(&self) -> Result<()> {
        self.sender.send(true)?;
        self.term.clear_line()?;
        self.term.show_cursor()?;
        self.term.flush()?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct CliOutput {
    inner: Arc<Mutex<CliOutputInner>>,
}

impl CliOutput {
    fn new(term: Term) -> (Self, JoinHandle<io::Result<()>>) {
        let (sender, receiver) = std::sync::mpsc::channel::<bool>();
        let inner = Arc::new(Mutex::new(CliOutputInner {
            sender,
            render_job: None,
            start_time: Instant::now(),
            term,
        }));
        let inner_for_handle = inner.clone();
        let handle = thread::spawn(move || {
            let inner_is_term = inner_for_handle.lock().unwrap().term.is_term();
            // If we're not in a real terminal, don't do anything; just wait until we're closed
            if !inner_is_term {
                return loop {
                    match receiver.recv() {
                        Ok(false) => {}
                        _ => break Ok(()),
                    }
                };
            }
            loop {
                match receiver.recv_timeout(Duration::from_secs_f64(1.0 / 30.0)) {
                    Ok(false) | Err(RecvTimeoutError::Timeout) => {}
                    _ => break Ok(()),
                }
                let inner = &mut *inner_for_handle.lock().unwrap();
                if let Some(job) = inner.render_job.as_ref() {
                    let mut job = job.lock();
                    let is_in_progress = {
                        let state = &*job.state.lock().unwrap();
                        matches!(
                            state,
                            RenderJobState::Paused
                                | RenderJobState::Rendering
                                | RenderJobState::Waiting
                        )
                    };
                    if is_in_progress {
                        let progress = job.update_progress(inner);

                        inner.term.clear_line()?;
                        inner.term.hide_cursor()?;

                        if let (Some(position), Some(duration)) =
                            (progress.position, progress.duration)
                        {
                            Self::draw_progress(
                                &mut inner.term,
                                position,
                                duration,
                                progress.progress,
                                progress.estimated_time_remaining,
                            )?;
                        }

                        inner.term.flush()?;
                    }
                }
            }
        });

        (Self { inner }, handle)
    }

    fn close(&self) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        inner.close()
    }

    fn set_render_job(&self, job: Option<SharedRenderJob>) {
        self.inner.lock().unwrap().render_job = job;
    }

    fn draw_progress(
        term: &mut Term,
        position: ClockTime,
        duration: ClockTime,
        progress: f64,
        eta: Option<f64>,
    ) -> io::Result<()> {
        let width = term.size().1 as usize;
        let mut progress_text = format!(
            "{:>.2} / {:>.2} | {:>3.0}%",
            position,
            duration,
            progress * 100.0
        );
        let eta = match eta {
            Some(eta) => {
                let mut label = String::new();
                format_eta(&mut label, eta, [["h", "h"], ["m", "m"], ["s", "s"]], "");
                label
            }
            None => String::from("?"),
        };
        write!(progress_text, " | {:>6}", eta).unwrap();
        let truncated_text = truncate_str(&progress_text, width, "…");
        let remaining_width = width
            .saturating_sub(measure_text_width(&truncated_text))
            .saturating_sub(1);
        write!(
            term,
            "{}{}{}",
            if remaining_width > 0 {
                Self::progress_bar(remaining_width, progress)
            } else {
                style(String::from(""))
            },
            if remaining_width > 0 { " " } else { "" },
            truncated_text
        )?;

        Ok(())
    }

    fn progress_bar(width: usize, progress: f64) -> StyledObject<String> {
        let mut bar = String::new();
        let completed_width = (width as f64 * progress).min(width as f64);
        let num_blocks = completed_width as usize;

        bar.push_str(&"█".repeat(num_blocks));
        if num_blocks < width {
            let block_part = (completed_width.fract() * 8.0) as usize;
            let partial_block = match block_part {
                0 => ' ',
                1 => '▏',
                2 => '▎',
                3 => '▍',
                4 => '▌',
                5 => '▋',
                6 => '▊',
                7 => '▉',
                _ => ' ',
            };
            bar.push(partial_block);
        }

        bar.push_str(&" ".repeat((width - num_blocks).saturating_sub(1)));

        console::style(bar).bg(console::Color::Color256(8))
    }
}

impl UIContext for CliOutput {
    fn request_repaint(&self) {
        self.inner.lock().unwrap().request_repaint()
    }

    fn current_time(&self) -> f64 {
        self.inner.lock().unwrap().current_time()
    }
}

impl UIContext for CliOutputInner {
    fn request_repaint(&self) {
        let _ = self.sender.send(false);
    }

    fn current_time(&self) -> f64 {
        (Instant::now() - self.start_time).as_secs_f64()
    }
}
