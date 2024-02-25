//! Dead simple command line utility to control the volume of your PulseAudio or PipeWire sound server.
//!
//! # Features
//!
//! * Can control the volume of your default input and output device.
//! * Shows a notification with the new volume.
//!   * Notifications include a progress bar if the notification daemon supports it!
//!
//! # Usage
//!
//! Usage information is available by running the program with the `--help` flag.
//!
//! ```sh
//! volume-ctl --help
//! ```

use libpulse_binding::context::{State, Context};
use libpulse_binding::error::PAErr;
use libpulse_binding::mainloop::standard::Mainloop;
use libpulse_binding::volume::{ChannelVolumes, Volume};
use std::sync::{Mutex, Arc};
use notify_rust::Notification;

/// Control the volume of your PulseAudio/PipeWire sound server.
#[derive(clap::Parser)]
#[clap(styles = clap_style())]
struct Options {
	/// Show more log messages.
	#[clap(long, short)]
	#[clap(global = true)]
	#[clap(action = clap::ArgAction::Count)]
	verbose: u8,

	/// Show less log messages.
	#[clap(long, short)]
	#[clap(global = true)]
	#[clap(action = clap::ArgAction::Count)]
	quiet: u8,

	/// The command to execute.
	#[clap(subcommand)]
	command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
	/// Control the volume of your output device (speakers, headphones, ...).
	Output {
		#[clap(subcommand)]
		command: VolumeCommand,
	},

	/// Control the volume of your input device (microphone, ...).
	Input {
		#[clap(subcommand)]
		command: VolumeCommand,
	}
}

#[derive(clap::Subcommand)]
enum VolumeCommand {
	/// Increase the volume by the given percentage.
	Up {
		/// The percentage to increase the volume by.
		#[clap(value_name = "PERCENTAGE")]
		value: f64,
	},
	/// Decrease the volume by the given percentage.
	Down {
		/// The percentage to decrease the volume by.
		#[clap(value_name = "PERCENTAGE")]
		value: f64,
	},
	/// Set the volume to the given percentage.
	Set {
		/// The percentage to set the volume to.
		#[clap(value_name = "PERCENTAGE")]
		value: f64,
	},
	/// Toggle between muted and unmuted.
	ToggleMute,
	/// Mute the volume.
	Mute,
	/// Unmute the volume.
	Unmute,
}

fn main() {
	if let Err(()) = do_main(clap::Parser::parse()) {
		std::process::exit(1);
	}
}

fn do_main(options: Options) -> Result<(), ()> {
	env_logger::Builder::new()
		.filter_module(module_path!(), log_level(options.verbose, options.quiet))
		.format_timestamp(None)
		.format_target(false)
		.parse_default_env()
		.init();

	let mut main_loop = Mainloop::new()
		.ok_or_else(|| eprintln!("Failed to initialize PulseAudio main loop."))?;
	let context = connect(&mut main_loop)?;

	match options.command {
		Command::Output { command } => run_output_command(&mut main_loop, &context, command),
		Command::Input { command } => run_input_command(&mut main_loop, &context, command),
	}
}

/// Run a volume command on the output device.
fn run_output_command(main_loop: &mut Mainloop, context: &Context, command: VolumeCommand) -> Result<(), ()> {
	let mut volumes = get_output_volumes(main_loop, context)
		.map_err(|e| log::error!("Failed to get output volume: {e}"))?;

	apply_volume_command(&mut volumes, &command);

	set_output_volumes(main_loop, context, &volumes.channels)
		.map_err(|e| log::error!("Failed to set output volume: {e}"))?;
	set_output_muted(main_loop, context, volumes.muted)
		.map_err(|e| log::error!("Failed to mute/unmute output volume: {e}"))?;

	show_notification("Volume", "audio-volume", 0x49adff07, &volumes);

	Ok(())
}

/// Run a volume command on the input device.
fn run_input_command(main_loop: &mut Mainloop, context: &Context, command: VolumeCommand) -> Result<(), ()> {
	let mut volumes = get_input_volumes(main_loop, context)
		.map_err(|e| log::error!("Failed to get input volume: {e}"))?;
	apply_volume_command(&mut volumes, &command);
	set_input_volumes(main_loop, context, &volumes.channels)
		.map_err(|e| log::error!("Failed to set input volume: {e}"))?;
	set_input_muted(main_loop, context, volumes.muted)
		.map_err(|e| log::error!("Failed to mute/unmute input volume: {e}"))?;

	show_notification("Microphone", "microphone-sensitivity", 0x49adff08, &volumes);

	Ok(())
}

/// Apply a [`VolumeCommand`] to a [`Volumes`] struct.
fn apply_volume_command(volumes: &mut Volumes, command: &VolumeCommand) {
	match command {
		VolumeCommand::Up { value } => {
			map_volumes(&mut volumes.channels, |x| x + value);
		}
		VolumeCommand::Down { value} => {
			map_volumes(&mut volumes.channels, |x| x - value);
		},
		VolumeCommand::Set { value } => {
			map_volumes(&mut volumes.channels, |_| *value);
		},
		VolumeCommand::Mute => {
			volumes.muted = true;
		},
		VolumeCommand::Unmute => {
			volumes.muted = false;
		},
		VolumeCommand::ToggleMute => {
			volumes.muted = !volumes.muted;
		},
	}
}

/// Convert a [`Volume`] to a percentage as `f64`.
fn volume_to_percentage(volume: Volume) -> f64 {
	let range = Volume::NORMAL.0 as f64 - Volume::MUTED.0 as f64;
	(volume.0 as f64 - Volume::MUTED.0 as f64) * 100.0 / range
}

/// Convert a percentage to a [`Volume`].
fn percentage_to_volume(factor: f64) -> Volume {
	let range = Volume::NORMAL.0 as f64 - Volume::MUTED.0 as f64;
	Volume((Volume::MUTED.0 as f64 + factor * range / 100.0) as u32)
}

/// Apply a function to all channel volumes.
fn map_volumes<F: FnMut(f64) -> f64>(volumes: &mut ChannelVolumes, mut action: F) {
	for volume in volumes.get_mut() {
		let factor = volume_to_percentage(*volume);
		let adjusted = action(factor).clamp(0.0, 125.0);
		*volume = percentage_to_volume(adjusted);
	}
}

/// Volume information for a input or output device.
struct Volumes {
	/// Is the device muted?
	muted: bool,
	/// The volumes of all channels of the device.
	channels: ChannelVolumes,
}

/// Get the volume of the output device.
fn get_output_volumes(main_loop: &mut Mainloop, context: &Context) -> Result<Volumes, PAErr> {
	run(main_loop, move |output| {
		context.introspect().get_sink_info_by_name("@DEFAULT_SINK@", move |info| {
			match info {
				libpulse_binding::callbacks::ListResult::Item(x) => {
					*output.lock().unwrap() = Some(Ok(Volumes {
						muted: x.mute,
						channels: x.volume,
					}));
				},
				libpulse_binding::callbacks::ListResult::End => {
				},
				libpulse_binding::callbacks::ListResult::Error => {
					*output.lock().unwrap() = Some(Err(()));
				},
			}
		});
	})?
	.map_err(|()| context.errno())
}

/// Get the volume of the output device.
fn set_output_volumes(main_loop: &mut Mainloop, context: &Context, volumes: &ChannelVolumes) -> Result<(), PAErr> {
	run(main_loop, move |output| {
		context.introspect().set_sink_volume_by_name("@DEFAULT_SINK@", volumes, Some(Box::new(move |success| {
			if success {
				*output.lock().unwrap() = Some(Ok(()));
			} else {
				*output.lock().unwrap() = Some(Err(()));
			}
		})));
	})?
	.map_err(|()| context.errno())
}

/// Set the muted state of the output device.
fn set_output_muted(main_loop: &mut Mainloop, context: &Context, muted: bool) -> Result<(), PAErr> {
	run(main_loop, move |output| {
		context.introspect().set_sink_mute_by_name("@DEFAULT_SINK@", muted, Some(Box::new(move |success| {
			if success {
				*output.lock().unwrap() = Some(Ok(()));
			} else {
				*output.lock().unwrap() = Some(Err(()));
			}
		})));
	})?
	.map_err(|()| context.errno())
}

/// Get the volume of the input device.
fn get_input_volumes(main_loop: &mut Mainloop, context: &Context) -> Result<Volumes, PAErr> {
	run(main_loop, move |output| {
		context.introspect().get_source_info_by_name("@DEFAULT_SOURCE@", move |info| {
			match info {
				libpulse_binding::callbacks::ListResult::Item(x) => {
					*output.lock().unwrap() = Some(Ok(Volumes {
						muted: x.mute,
						channels: x.volume,
					}));
				},
				libpulse_binding::callbacks::ListResult::End => {
				},
				libpulse_binding::callbacks::ListResult::Error => {
					*output.lock().unwrap() = Some(Err(()));
				},
			}
		});
	})?
	.map_err(|()| context.errno())
}

/// Set the volume of the input device.
fn set_input_volumes(main_loop: &mut Mainloop, context: &Context, volumes: &ChannelVolumes) -> Result<(), PAErr> {
	run(main_loop, move |output| {
		context.introspect().set_source_volume_by_name("@DEFAULT_SOURCE@", volumes, Some(Box::new(move |success| {
			if success {
				*output.lock().unwrap() = Some(Ok(()));
			} else {
				*output.lock().unwrap() = Some(Err(()));
			}
		})));
	})?
	.map_err(|()| context.errno())
}

/// Set the muted state of the input device.
fn set_input_muted(main_loop: &mut Mainloop, context: &Context, muted: bool) -> Result<(), PAErr> {
	run(main_loop, move |output| {
		context.introspect().set_source_mute_by_name("@DEFAULT_SOURCE@", muted, Some(Box::new(move |success| {
			if success {
				*output.lock().unwrap() = Some(Ok(()));
			} else {
				*output.lock().unwrap() = Some(Err(()));
			}
		})));
	})?
	.map_err(|()| context.errno())
}

/// Connect to a PulseAudio or PipeWire sound server.
fn connect(main_loop: &mut Mainloop) -> Result<Context, ()> {
	// Create the context.
	let mut context = libpulse_binding::context::Context::new(main_loop, "volume-control")
		.ok_or_else(|| eprintln!("Failed initialize PulseAudio context."))?;
	log::debug!("Protocol version: {}", context.get_protocol_version());
	log::debug!("Context state: {:?}", context.get_state());

	// Initiate the connection.
	context.connect(None, libpulse_binding::context::FlagSet::NOFLAGS, None)
		.map_err(|e| eprintln!("Failed to connect to PulseAudio server: {e}"))?;
	log::debug!("Context state: {:?}", context.get_state());

	// Run the main loop until the connection succeeded or failed.
	run_until(main_loop, |_main_loop| {
		let state = context.get_state();
		log::debug!("Context state: {:?}", state);
		match state {
			State::Ready => true,
			State::Failed => true,
			State::Unconnected => true,
			State::Terminated => true,
			State::Connecting => false,
			State::Authorizing => false,
			State::SettingName => false,
		}
	})
	.map_err(|e| log::error!("Error in PulseAudio main loop: {e}"))?;

	// Check the end state to see if we connected successfully.
	let state = context.get_state();
	match state {
		State::Ready => (),
		State::Failed => {
			log::error!("Failed to connect to PulseAudio server: {}", context.errno());
			return Err(());
		},
		| State::Unconnected
		| State::Terminated
		| State::Connecting
		| State::Authorizing
		| State::SettingName => {
			log::error!("PulseAudio context in unexpected state: {state:?}");
			log::error!("Last error: {}", context.errno());
			return Err(());
		}
	}
	Ok(context)
}

/// Run the libpulse main loop until a condition becomes true.
fn run_until<F>(main_loop: &mut Mainloop, condition: F) -> Result<Option<i32>, PAErr>
where
	F: Fn(&mut Mainloop) -> bool,
{
	use libpulse_binding::mainloop::standard::IterateResult;
	loop {
		match main_loop.iterate(true) {
			IterateResult::Err(e) => {
				return Err(e);
			},
			IterateResult::Quit(code) => {
				return Ok(Some(code.0));
			},
			IterateResult::Success(_iterations) => (),
		}
		if condition(main_loop) {
			return Ok(None)
		};
	}
}

/// Run the libpulse main loop until a value is set.
fn run<F, T>(main_loop: &mut Mainloop, operation: F) -> Result<T, PAErr>
where
	F: FnOnce(Arc<Mutex<Option<T>>>),
{
	use libpulse_binding::mainloop::standard::IterateResult;
	let output = Arc::new(Mutex::new(None));
	operation(output.clone());

	loop {
		if let Some(value) = output.lock().unwrap().take() {
			return Ok(value);
		}
		match main_loop.iterate(true) {
			IterateResult::Err(e) => {
				return Err(e);
			},
			IterateResult::Quit(code) => {
				std::process::exit(code.0);
			},
			IterateResult::Success(_iterations) => (),
		}
	}
}

/// Show a notification about the new sound server state.
fn show_notification(name: &str, icon_prefix: &str, id: u32, volumes: &Volumes) {
	let max_volume = volume_to_percentage(volumes.channels.max());
	let mut notification = Notification::new();
	if volumes.muted {
		notification.summary(&format!("{name}: muted ({max_volume:.0}%)"));
	} else {
		notification.summary(&format!("{name}: {max_volume:.0}%"));
	}
	if volumes.muted {
		notification.icon(&format!("{icon_prefix}-muted"));
	} else if max_volume <= 100.0 / 3.0 {
		notification.icon(&format!("{icon_prefix}-low"));
	} else if max_volume < 100.0 * 2.0 / 3.0 {
		notification.icon(&format!("{icon_prefix}-medium"));
	} else {
		notification.icon(&format!("{icon_prefix}-high"));
	}
	notification.id(id);
	notification.hint(notify_rust::Hint::CustomInt("value".to_owned(), max_volume.round() as i32));
	notification.show()
		.map_err(|e| log::warn!("Failed to show notification: {e}"))
		.ok();
}

/// Create a colorful style for the command line interface.
fn clap_style() -> clap::builder::Styles {
	use clap::builder::styling::AnsiColor;
	clap::builder::Styles::styled()
		.header(AnsiColor::Yellow.on_default())
		.usage(AnsiColor::Green.on_default())
		.literal(AnsiColor::Green.on_default())
		.placeholder(AnsiColor::Green.on_default())
}

/// Determine the log level filter based on the --verbose/-v and --quiet/-q flags.
fn log_level(verbose: u8, quiet: u8) -> log::LevelFilter {
	match i16::from(verbose) -  i16::from(quiet) {
		..=-2 => log::LevelFilter::Error,
		-1 => log::LevelFilter::Warn,
		0 => log::LevelFilter::Info,
		1 => log::LevelFilter::Debug,
		2.. => log::LevelFilter::Trace,
	}
}
