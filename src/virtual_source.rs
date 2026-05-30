use anyhow::{Context as _, Result, bail};
use libpulse_binding as pulse;
use pulse::context::{Context, FlagSet, State};
use pulse::mainloop::standard::{IterateResult, Mainloop};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

pub struct VirtualSource {
    mainloop: Mainloop,
    context: Context,
    source_module: Option<u32>,
    sink_module: Option<u32>,
    previous_pulse_sink: Option<String>,
}

impl VirtualSource {
    pub fn create(
        source_name: &str,
        sink_name: Option<&str>,
        source_description: Option<&str>,
    ) -> Result<Self> {
        validate_name("source", source_name)?;
        let source_description = source_description.unwrap_or("iOS Mic");
        validate_description(source_description)?;
        let sink_name = sink_name
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("{source_name}_sink"));
        validate_name("sink", &sink_name)?;

        let mut mainloop = Mainloop::new().context("failed to create PulseAudio main loop")?;
        let mut context =
            Context::new(&mainloop, "iosmic").context("failed to create PulseAudio context")?;
        context
            .connect(None, FlagSet::NOFLAGS, None)
            .context("failed to connect to the PulseAudio/PipeWire server")?;
        wait_for_context(&mut mainloop, &context)?;

        let sink_module = load_module(
            &mut mainloop,
            &mut context,
            "module-null-sink",
            &format!(
                "sink_name={sink_name} sink_properties={}",
                module_properties(&format!("{source_description} Output")),
            ),
        )
        .with_context(|| format!("failed to create backing sink '{sink_name}'"))?;

        let source_module = match load_module(
            &mut mainloop,
            &mut context,
            "module-remap-source",
            &format!(
                "master={sink_name}.monitor source_name={source_name} source_properties={}",
                module_properties(source_description),
            ),
        ) {
            Ok(module) => module,
            Err(error) => {
                let _ = unload_module(&mut mainloop, &mut context, sink_module);
                return Err(error)
                    .with_context(|| format!("failed to create source '{source_name}'"));
            }
        };

        let previous_pulse_sink = std::env::var("PULSE_SINK").ok();
        // The ALSA pulse plugin reads this variable when AlsaSink opens the `pulse` device.
        unsafe { std::env::set_var("PULSE_SINK", &sink_name) };

        eprintln!(
            "Created source '{source_description}' ({source_name}) backed by sink '{sink_name}'"
        );
        eprintln!("Select '{source_description}' as the microphone in applications.");

        Ok(Self {
            mainloop,
            context,
            source_module: Some(source_module),
            sink_module: Some(sink_module),
            previous_pulse_sink,
        })
    }

    fn cleanup(&mut self) {
        if let Some(module) = self.source_module.take()
            && let Err(error) = unload_module(&mut self.mainloop, &mut self.context, module)
        {
            eprintln!("Warning: failed to unload virtual source: {error:#}");
        }
        if let Some(module) = self.sink_module.take()
            && let Err(error) = unload_module(&mut self.mainloop, &mut self.context, module)
        {
            eprintln!("Warning: failed to unload virtual sink: {error:#}");
        }

        match &self.previous_pulse_sink {
            Some(value) => unsafe { std::env::set_var("PULSE_SINK", value) },
            None => unsafe { std::env::remove_var("PULSE_SINK") },
        }
        self.context.disconnect();
    }
}

impl Drop for VirtualSource {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn validate_name(kind: &str, name: &str) -> Result<()> {
    if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Ok(());
    }
    bail!("{kind} name may only contain ASCII letters, numbers, '.', '_', and '-'")
}

fn validate_description(description: &str) -> Result<()> {
    if !description.is_empty() && description.chars().all(|character| !character.is_control()) {
        return Ok(());
    }
    bail!("source description must be non-empty and cannot contain control characters")
}

fn module_properties(description: &str) -> String {
    let property_value = description.replace('\\', "\\\\").replace('"', "\\\"");
    let property_list = format!("device.description=\"{property_value}\"");
    let module_value = property_list.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{module_value}'")
}

fn wait_for_context(mainloop: &mut Mainloop, context: &Context) -> Result<()> {
    let deadline = Instant::now() + OPERATION_TIMEOUT;
    loop {
        match context.get_state() {
            State::Ready => return Ok(()),
            State::Failed | State::Terminated => {
                bail!("PulseAudio/PipeWire connection failed: {}", context.errno())
            }
            _ if Instant::now() >= deadline => bail!("timed out connecting to PulseAudio/PipeWire"),
            _ => iterate(mainloop)?,
        }
    }
}

fn load_module(
    mainloop: &mut Mainloop,
    context: &mut Context,
    name: &str,
    argument: &str,
) -> Result<u32> {
    let result = Rc::new(Cell::new(None));
    let callback_result = Rc::clone(&result);
    let _operation = context
        .introspect()
        .load_module(name, argument, move |index| {
            callback_result.set(Some(index));
        });
    let index = wait_for_result(mainloop, context, &result, "loading module")?;
    if index == pulse::def::INVALID_INDEX {
        bail!(
            "PulseAudio/PipeWire rejected module load: {}",
            context.errno()
        )
    }
    Ok(index)
}

fn unload_module(mainloop: &mut Mainloop, context: &mut Context, index: u32) -> Result<()> {
    let result = Rc::new(Cell::new(None));
    let callback_result = Rc::clone(&result);
    let _operation = context.introspect().unload_module(index, move |success| {
        callback_result.set(Some(success));
    });
    if wait_for_result(mainloop, context, &result, "unloading module")? {
        Ok(())
    } else {
        bail!("PulseAudio/PipeWire rejected module unload")
    }
}

fn wait_for_result<T: Copy>(
    mainloop: &mut Mainloop,
    context: &Context,
    result: &Cell<Option<T>>,
    operation: &str,
) -> Result<T> {
    let deadline = Instant::now() + OPERATION_TIMEOUT;
    loop {
        if let Some(value) = result.get() {
            return Ok(value);
        }
        match context.get_state() {
            State::Failed | State::Terminated => bail!("{operation} failed: {}", context.errno()),
            _ if Instant::now() >= deadline => bail!("timed out while {operation}"),
            _ => iterate(mainloop)?,
        }
    }
}

fn iterate(mainloop: &mut Mainloop) -> Result<()> {
    match mainloop.iterate(false) {
        IterateResult::Success(_) => {
            std::thread::sleep(Duration::from_millis(10));
            Ok(())
        }
        IterateResult::Quit(_) => bail!("PulseAudio/PipeWire main loop stopped"),
        IterateResult::Err(error) => Err(anyhow::Error::msg(format!("{error:?}")))
            .context("PulseAudio/PipeWire main loop failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::{module_properties, validate_description, validate_name};

    #[test]
    fn accepts_pulse_audio_names_used_by_the_helper() {
        assert!(validate_name("source", "ios.mic_1-test").is_ok());
    }

    #[test]
    fn rejects_unsafe_pulse_audio_names() {
        assert!(validate_name("source", "ios mic").is_err());
        assert!(validate_name("source", "").is_err());
        assert!(validate_name("source", "ios=mic").is_err());
    }

    #[test]
    fn accepts_human_readable_descriptions() {
        assert!(validate_description("iOS Mic").is_ok());
        assert!(validate_description("'s iPhone microphone").is_ok());
        assert!(validate_description("\n").is_err());
    }

    #[test]
    fn escapes_descriptions_for_module_and_proplist_parsers() {
        assert_eq!(
            module_properties("'s \"iOS Mic\""),
            r#"'device.description="\'s \\"iOS Mic\\""'"#
        );
    }
}
