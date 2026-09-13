use std::collections::HashMap;

/// One benchmark result, in nanoseconds per `bench()` call.
pub struct Sample {
    pub name: String,
    pub median_ns: u64,
    pub min_ns: u64,
    pub samples: usize,
}

pub struct Config {
    pub samples: usize,
    pub budget: std::time::Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            samples: 15,
            budget: std::time::Duration::from_millis(20),
        }
    }
}

/// Summarizes raw per-sample nanoseconds into median/min.
pub fn summarize(name: String, mut results: Vec<u64>) -> Sample {
    results.sort_unstable();
    let median_ns = results[results.len() / 2];
    let min_ns = results[0];
    Sample {
        name,
        median_ns,
        min_ns,
        samples: results.len(),
    }
}

/// Parses `--samples N` / `--samples=N` and `--budget-ms N` / `--budget-ms=N`
/// from a forward-slash-free argv tail, returning the flags and the remaining
/// positional arguments.
pub fn parse_flags(args: &[String]) -> Result<(Config, Vec<String>), String> {
    let mut config = Config::default();
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--samples" {
            index += 1;
            config.samples = args
                .get(index)
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| "--samples needs a value".to_owned())?;
        } else if let Some(value) = arg.strip_prefix("--samples=") {
            config.samples = value
                .parse()
                .map_err(|_| "--samples needs a value".to_owned())?;
        } else if arg == "--budget-ms" {
            index += 1;
            let value: u64 = args
                .get(index)
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| "--budget-ms needs a value".to_owned())?;
            config.budget = std::time::Duration::from_millis(value);
        } else if let Some(value) = arg.strip_prefix("--budget-ms=") {
            let value: u64 = value
                .parse()
                .map_err(|_| "--budget-ms needs a value".to_owned())?;
            config.budget = std::time::Duration::from_millis(value);
        } else if arg == "--workers" {
            // Accepted for parity with the Odin driver; the driver's worker
            // count is fixed at session open.
            index += 1;
        } else if arg.starts_with("--workers=") {
        } else if arg.starts_with("--") {
            return Err(format!("unknown flag: {arg}"));
        } else {
            positional.push(arg.clone());
        }
        index += 1;
    }
    if config.samples == 0 {
        return Err("--samples must be positive".to_owned());
    }
    Ok((config, positional))
}

/// Calibrates an inner repeat count so one sample spans roughly `budget`.
pub fn calibrate(one_call_ns: u64, budget: std::time::Duration) -> usize {
    if one_call_ns == 0 {
        return 1;
    }
    let target = budget.as_nanos() as u64;
    (target / one_call_ns).max(1) as usize
}

/// Collects name -> median for joining results across implementations.
pub fn medians(samples: &[Sample]) -> HashMap<String, u64> {
    samples
        .iter()
        .map(|sample| (sample.name.clone(), sample.median_ns))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse_equals_and_space_forms() {
        let args = vec![
            "--samples=7".to_owned(),
            "--budget-ms".to_owned(),
            "5".to_owned(),
            "a.mica".to_owned(),
        ];
        let (config, positional) = parse_flags(&args).unwrap();
        assert_eq!(config.samples, 7);
        assert_eq!(config.budget, std::time::Duration::from_millis(5));
        assert_eq!(positional, vec!["a.mica"]);
    }

    #[test]
    fn calibration_uses_target_and_floor() {
        assert_eq!(calibrate(0, std::time::Duration::from_millis(20)), 1);
        assert_eq!(calibrate(2_000_000, std::time::Duration::from_millis(20)), 10);
        assert_eq!(calibrate(1_000_000_000, std::time::Duration::from_millis(1)), 1);
    }

    #[test]
    fn summarize_picks_median_and_min() {
        let sample = summarize("x".to_owned(), vec![30, 10, 20, 40, 25]);
        assert_eq!(sample.median_ns, 25);
        assert_eq!(sample.min_ns, 10);
    }
}
