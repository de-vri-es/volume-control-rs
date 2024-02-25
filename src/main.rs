use libpulse_binding::context::{State, Context};
use libpulse_binding::error::PAErr;
use libpulse_binding::mainloop::standard::Mainloop;
use libpulse_binding::volume::{ChannelVolumes, Volume};
use std::sync::{Mutex, Arc};
use notify_rust::Notification;

#[derive(clap::Parser)]
struct Options {
	#[clap(subcommand)]
	command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
	Up {
		value: f64,
	},
	Down {
		value: f64,
	},
	Set {
		value: f64,
	},
	Mute,
	Unmute,
	ToggleMute,
}

fn main() {
	if let Err(()) = do_main(clap::Parser::parse()) {
		std::process::exit(1);
	}
}

fn do_main(options: Options) -> Result<(), ()> {
	let mut main_loop = Mainloop::new()
		.ok_or_else(|| eprintln!("Failed to initialize PulseAudio main loop."))?;

	let mut context = libpulse_binding::context::Context::new(&main_loop, "volume-control")
		.ok_or_else(|| eprintln!("Failed initialize PulseAudio context."))?;
	eprintln!("Protocol version: {}", context.get_protocol_version());
	eprintln!("Context state: {:?}", context.get_state());
	context.connect(None, libpulse_binding::context::FlagSet::NOFLAGS, None)
		.map_err(|e| eprintln!("Failed to connect to PulseAudio server: {e}"))?;
	eprintln!("Context state: {:?}", context.get_state());

	run_until(&mut main_loop, |_main_loop| {
		match context.get_state() {
			State::Ready => true,
			State::Failed => true,
			State::Unconnected => true,
			State::Terminated => true,
			State::Connecting => false,
			State::Authorizing => false,
			State::SettingName => false,
		}
	})
	.map_err(|e| eprintln!("Error in PulseAudio main loop: {e}"))?;

	let state = context.get_state();
	match state {
		State::Ready => (),
		State::Failed => {
			eprintln!("Failed to connect to PulseAudio server: {}", context.errno());
			return Err(());
		},
		| State::Unconnected
			| State::Terminated
			| State::Connecting
			| State::Authorizing
			| State::SettingName => {
				eprintln!("PulseAudio context in unexpected state: {state:?}");
				eprintln!("Lasts error: {}", context.errno());
				return Err(());
			}
	}

	eprintln!("Context state: {:?}", context.get_state());

	let mut volumes = get_volumes(&mut main_loop, &context)
		.map_err(|e| eprintln!("Failed to get default sink information: {e}"))?;
	let mut muted = get_muted(&mut main_loop, &context)
		.map_err(|e| eprintln!("Failed to get muted state of default sink: {e}"))?;

	match options.command {
		Command::Up { value } => {
			map_volumes(&mut volumes, |x| x + value);
			set_volumes(&mut main_loop, &context, &volumes)
				.map_err(|e| eprintln!("Failed to set volume: {e}"))?;
		}
		Command::Down { value} => {
			map_volumes(&mut volumes, |x| x - value);
			set_volumes(&mut main_loop, &context, &volumes)
				.map_err(|e| eprintln!("Failed to set volume: {e}"))?;
		},
		Command::Set { value } => {
			map_volumes(&mut volumes, |_| value);
			set_volumes(&mut main_loop, &context, &volumes)
				.map_err(|e| eprintln!("Failed to set volume: {e}"))?;
		},
		Command::Mute => {
			muted = true;
			set_muted(&mut main_loop, &context, muted)
				.map_err(|e| eprintln!("Failed to mute volume: {e}"))?;
		},
		Command::Unmute => {
			muted = false;
			set_muted(&mut main_loop, &context, muted)
				.map_err(|e| eprintln!("Failed to unmute volume: {e}"))?;
		},
		Command::ToggleMute => {
			muted = !muted;
			set_muted(&mut main_loop, &context, muted)
				.map_err(|e| eprintln!("Failed to mute/unmute volume: {e}"))?;
		},
	}

	let max_volume = volume_to_percentage(volumes.max());
	let mut notification = Notification::new();
	if muted {
		notification.summary(&format!("Volume: muted ({:.0}%)", max_volume));
	} else {
		notification.summary(&format!("Volume: {:.0}%", max_volume));
	}
	if muted {
		notification.icon("audio-volume-muted");
	} else if max_volume <= 100.0 / 3.0 {
		notification.icon("audio-volume-low");
	} else if max_volume < 100.0 * 2.0 / 3.0 {
		notification.icon("audio-volume-medium");
	} else {
		notification.icon("audio-volume-high");
	}
	notification.id(0x49adff07);
	notification.hint(notify_rust::Hint::CustomInt("value".to_owned(), max_volume.round() as i32));
	notification.hint(notify_rust::Hint::CustomInt("progress".to_owned(), max_volume.round() as i32));
	notification.hint(notify_rust::Hint::Custom("progress-label".to_owned(), "volume".to_owned()));
	notification.show()
		.map_err(|e| eprintln!("Failed to show notification: {e}"))
		.ok();

	Ok(())
}

fn volume_to_percentage(volume: Volume) -> f64 {
	let range = Volume::NORMAL.0 as f64 - Volume::MUTED.0 as f64;
	(volume.0 as f64 - Volume::MUTED.0 as f64) * 100.0 / range
}

fn percentage(factor: f64) -> Volume {
	let range = Volume::NORMAL.0 as f64 - Volume::MUTED.0 as f64;
	Volume((Volume::MUTED.0 as f64 + factor * range / 100.0) as u32)
}

fn map_volumes<F: FnMut(f64) -> f64>(volumes: &mut ChannelVolumes, mut action: F) {
	for volume in volumes.get_mut() {
		let factor = volume_to_percentage(*volume);
		let adjusted = action(factor).clamp(0.0, 125.0);
		*volume = percentage(adjusted);
	}
}

fn get_volumes(main_loop: &mut Mainloop, context: &Context) -> Result<ChannelVolumes, PAErr> {
	run(main_loop, move |output| {
		context.introspect().get_sink_info_by_name("@DEFAULT_SINK@", move |info| {
			match info {
				libpulse_binding::callbacks::ListResult::Item(x) => {
					*output.lock().unwrap() = Some(Ok(x.volume));
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

fn set_volumes(main_loop: &mut Mainloop, context: &Context, volumes: &ChannelVolumes) -> Result<(), PAErr> {
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

fn get_muted(main_loop: &mut Mainloop, context: &Context) -> Result<bool, PAErr> {
	run(main_loop, move |output| {
		context.introspect().get_sink_info_by_name("@DEFAULT_SINK@", move |info| {
			match info {
				libpulse_binding::callbacks::ListResult::Item(x) => {
					*output.lock().unwrap() = Some(Ok(x.mute));
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

fn set_muted(main_loop: &mut Mainloop, context: &Context, muted: bool) -> Result<(), PAErr> {
	run(main_loop, move |output| {
		context.introspect().set_sink_mute_by_name("@DEFAULT_SINK@", muted, Some(Box::new(move |success| {
			eprintln!("set_sink_mute_by_name({muted}): {success}");
			if success {
				*output.lock().unwrap() = Some(Ok(()));
			} else {
				*output.lock().unwrap() = Some(Err(()));
			}
		})));
	})?
	.map_err(|()| context.errno())
}

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
