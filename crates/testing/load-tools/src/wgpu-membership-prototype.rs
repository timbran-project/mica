// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
//
// This program is free software: you can redistribute it and/or modify it under
// the terms of the GNU Affero General Public License as published by the Free
// Software Foundation, version 3.

//! Compares CPU and native-wgpu execution of a `Tuple` membership filter.
//!
//! The operator models an indexed relational probe: each row in the left input
//! probes a sorted right input and matching left tuples are materialized. The
//! GPU path explicitly columnizes immediate `Value`s and encodes one homogeneous
//! probe domain into fixed-width words; it does not depend on `Value`'s private
//! representation.

use clap::{Parser, ValueEnum};
use mica_relation_kernel::{PackedRelation, Tuple};
use mica_var::{Identity, Value};
use rayon::prelude::*;
use std::hint::black_box;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt;

const WORKGROUP_SIZE: u32 = 256;
const TIMESTAMP_BYTES: u64 = 16;

const MEMBERSHIP_SHADER: &str = r#"
struct Params {
    left_len: u32,
    right_len: u32,
    _padding_0: u32,
    _padding_1: u32,
}

@group(0) @binding(0)
var<storage, read> left_rows: array<u64>;

@group(0) @binding(1)
var<storage, read> right_rows: array<u64>;

@group(0) @binding(2)
var<storage, read_write> matches: array<u32>;

@group(0) @binding(3)
var<uniform> params: Params;

@compute @workgroup_size(256)
fn membership(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let row = invocation.x;
    if row >= params.left_len {
        return;
    }

    let probe = left_rows[row];
    var lower = 0u;
    var upper = params.right_len;
    loop {
        if lower >= upper {
            break;
        }
        let middle = lower + ((upper - lower) / 2u);
        let candidate = right_rows[middle];
        if candidate < probe {
            lower = middle + 1u;
        } else {
            upper = middle;
        }
    }
    matches[row] = select(0u, 1u, lower < params.right_len && right_rows[lower] == probe);
}
"#;

#[derive(Clone, Debug, Parser)]
#[command(
    name = "wgpu-membership-prototype",
    about = "Compare packed relation membership on CPU, Rayon, and native wgpu"
)]
struct Args {
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "16384,65536,262144,1048576,4194304"
    )]
    rows: Vec<usize>,

    #[arg(long, default_value_t = 12)]
    iterations: usize,

    #[arg(long, default_value_t = 3)]
    cold_iterations: usize,

    #[arg(long, default_value_t = 3)]
    warmup_iterations: usize,

    #[arg(long, default_value_t = false)]
    allow_software_adapter: bool,

    #[arg(long, value_enum, default_value_t = ProbeOrder::Mixed)]
    probe_order: ProbeOrder,

    #[arg(long, value_enum, value_delimiter = ',', default_value = "half")]
    match_rates: Vec<MatchRate>,

    #[arg(long, value_enum, default_value_t = ProbeDomain::Int)]
    value_domain: ProbeDomain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ProbeOrder {
    Sequential,
    Mixed,
}

impl ProbeOrder {
    fn label(self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum MatchRate {
    None,
    Half,
    All,
}

impl MatchRate {
    fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Half => "half",
            Self::All => "all",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ProbeDomain {
    Int,
    Identity,
    Float,
}

impl ProbeDomain {
    fn label(self) -> &'static str {
        match self {
            Self::Int => "int",
            Self::Identity => "identity",
            Self::Float => "float",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GpuBufferMode {
    StagedReadback,
    SharedMappable,
}

impl GpuBufferMode {
    fn label(self) -> &'static str {
        match self {
            Self::StagedReadback => "wgpu_staged",
            Self::SharedMappable => "wgpu_shared",
        }
    }
}

#[derive(Debug)]
struct GpuSample {
    wall: Duration,
    kernel: Duration,
    columnize: Duration,
    encode: Duration,
    rows: Vec<Tuple>,
}

struct PackedGpuInputs {
    left: Vec<u64>,
    right: Vec<u64>,
    columnize: Duration,
    encode: Duration,
}

#[derive(Clone, Copy)]
struct WarmIterations {
    warmup: usize,
    measured: usize,
}

struct GpuPrototype {
    adapter_info: wgpu::AdapterInfo,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    query_set: wgpu::QuerySet,
    timestamp_resolve: wgpu::Buffer,
    timestamp_readback: wgpu::Buffer,
    shared_mappable: bool,
}

struct OperatorBuffers {
    bind_group: wgpu::BindGroup,
    output: wgpu::Buffer,
    readback: Option<wgpu::Buffer>,
    row_count: usize,
}

#[derive(Clone, Copy, Debug)]
struct WorkloadShape {
    probe_order: ProbeOrder,
    match_rate: MatchRate,
    value_domain: ProbeDomain,
    rows: usize,
    hits: usize,
}

fn main() -> Result<(), String> {
    let args = Args::parse();
    validate_args(&args)?;
    if cfg!(target_endian = "big") {
        return Err("the prototype currently requires a little-endian host".to_owned());
    }

    let gpu = GpuPrototype::new(args.allow_software_adapter)?;
    println!(
        "adapter={:?} backend={:?} device_type={:?} shared_mappable={}",
        gpu.adapter_info.name,
        gpu.adapter_info.backend,
        gpu.adapter_info.device_type,
        gpu.shared_mappable,
    );
    println!(
        "value_domain,probe_order,match_rate,rows,hits,backend,residency,columnize_us,encode_us,host_us,kernel_us,speedup_vs_serial"
    );

    for &match_rate in &args.match_rates {
        for &row_count in &args.rows {
            run_size(&gpu, &args, row_count, match_rate)?;
        }
    }
    Ok(())
}

fn validate_args(args: &Args) -> Result<(), String> {
    if args.rows.is_empty() || args.rows.contains(&0) {
        return Err("--rows must contain only non-zero sizes".to_owned());
    }
    if args.match_rates.is_empty() {
        return Err("--match-rates must not be empty".to_owned());
    }
    if args.rows.iter().any(|&rows| rows > u32::MAX as usize) {
        return Err("--rows values must fit in u32".to_owned());
    }
    if args.iterations == 0 || args.cold_iterations == 0 {
        return Err("iteration counts must be non-zero".to_owned());
    }
    Ok(())
}

fn run_size(
    gpu: &GpuPrototype,
    args: &Args,
    row_count: usize,
    match_rate: MatchRate,
) -> Result<(), String> {
    let (left, right) = make_inputs(row_count, args.probe_order, match_rate, args.value_domain);
    let expected = membership_serial(&left, &right);
    let shape = WorkloadShape {
        probe_order: args.probe_order,
        match_rate,
        value_domain: args.value_domain,
        rows: row_count,
        hits: expected.len(),
    };
    let serial = benchmark_cpu(
        args.iterations,
        || membership_serial(&left, &right),
        &expected,
    );

    black_box(membership_rayon(&left, &right));
    let parallel = benchmark_cpu(
        args.iterations,
        || membership_rayon(&left, &right),
        &expected,
    );

    print_cpu_result(shape, "cpu_serial", serial, serial);
    print_cpu_result(shape, "cpu_rayon", parallel, serial);

    for mode in gpu.supported_modes() {
        let cold = gpu.benchmark_cold(
            mode,
            &left,
            &right,
            &expected,
            args.value_domain,
            args.cold_iterations,
        )?;
        print_gpu_result(shape, mode, "cold", &cold, serial);

        let warm = gpu.benchmark_warm(
            mode,
            &left,
            &right,
            &expected,
            args.value_domain,
            WarmIterations {
                warmup: args.warmup_iterations,
                measured: args.iterations,
            },
        )?;
        print_gpu_result(shape, mode, "warm", &warm, serial);
    }
    Ok(())
}

fn make_inputs(
    rows: usize,
    order: ProbeOrder,
    match_rate: MatchRate,
    domain: ProbeDomain,
) -> (Vec<Tuple>, Vec<Tuple>) {
    let left = (0..rows)
        .map(|row| {
            let index = match order {
                ProbeOrder::Sequential => row as u64,
                ProbeOrder::Mixed => splitmix64(row as u64) % rows as u64,
            };
            let probe = match match_rate {
                MatchRate::None => index * 4 + 2,
                MatchRate::Half => match order {
                    ProbeOrder::Sequential => (row as u64) * 2,
                    ProbeOrder::Mixed => splitmix64(row as u64) % ((rows as u64) * 2) * 2,
                },
                MatchRate::All => index * 4,
            };
            Tuple::from([
                Value::int(row as i64).expect("row id should fit in a Mica integer"),
                probe_value(domain, probe),
            ])
        })
        .collect();
    let right = (0..rows)
        .map(|row| Tuple::from([probe_value(domain, (row as u64) * 4)]))
        .collect();
    (left, right)
}

fn probe_value(domain: ProbeDomain, value: u64) -> Value {
    match domain {
        ProbeDomain::Int => Value::int(value as i64).expect("probe should fit in a Mica integer"),
        ProbeDomain::Identity => {
            Value::identity(Identity::new(value).expect("probe should fit in a Mica identity"))
        }
        ProbeDomain::Float => {
            Value::float(value as f32).expect("probe should fit in a finite binary32")
        }
    }
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn membership_serial(left: &[Tuple], right: &[Tuple]) -> Vec<Tuple> {
    left.iter()
        .filter(|tuple| {
            let probe = &tuple.values()[1];
            right
                .binary_search_by(|candidate| candidate.values()[0].cmp(probe))
                .is_ok()
        })
        .cloned()
        .collect()
}

fn membership_rayon(left: &[Tuple], right: &[Tuple]) -> Vec<Tuple> {
    left.par_iter()
        .filter(|tuple| {
            let probe = &tuple.values()[1];
            right
                .binary_search_by(|candidate| candidate.values()[0].cmp(probe))
                .is_ok()
        })
        .cloned()
        .collect()
}

fn benchmark_cpu(
    iterations: usize,
    mut execute: impl FnMut() -> Vec<Tuple>,
    expected: &[Tuple],
) -> Duration {
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        let rows = black_box(execute());
        samples.push(started.elapsed());
        assert_eq!(rows, expected);
    }
    median_duration(&mut samples)
}

fn pack_gpu_inputs(
    left: &[Tuple],
    right: &[Tuple],
    domain: ProbeDomain,
) -> Result<PackedGpuInputs, String> {
    let columnize_started = Instant::now();
    let left = PackedRelation::from_canonical_tuples(left.to_vec(), 2).ok_or_else(|| {
        "left tuples cannot be represented as an immediate packed relation".to_owned()
    })?;
    let right = PackedRelation::from_canonical_tuples(right.to_vec(), 1).ok_or_else(|| {
        "right tuples cannot be represented as an immediate packed relation".to_owned()
    })?;
    let columnize = columnize_started.elapsed();

    let encode_started = Instant::now();
    let left = encode_column(&left.columns()[1], domain)?;
    let right = encode_column(&right.columns()[0], domain)?;
    let encode = encode_started.elapsed();
    if !right.windows(2).all(|window| window[0] < window[1]) {
        return Err("encoded right probe column is not strictly ordered".to_owned());
    }
    Ok(PackedGpuInputs {
        left,
        right,
        columnize,
        encode,
    })
}

fn encode_column(values: &[Value], domain: ProbeDomain) -> Result<Vec<u64>, String> {
    values
        .iter()
        .map(|value| encode_value(value, domain))
        .collect()
}

fn encode_value(value: &Value, domain: ProbeDomain) -> Result<u64, String> {
    match domain {
        ProbeDomain::Int => value
            .as_int()
            .map(|value| (value as u64) ^ (1 << 63))
            .ok_or_else(|| format!("expected int probe value, received {value:?}")),
        ProbeDomain::Identity => value
            .as_identity()
            .map(Identity::raw)
            .ok_or_else(|| format!("expected identity probe value, received {value:?}")),
        ProbeDomain::Float => value
            .as_float()
            .map(|value| {
                let bits = value.to_bits();
                if (bits & 0x8000_0000) != 0 {
                    u64::from(!bits)
                } else {
                    u64::from(bits ^ 0x8000_0000)
                }
            })
            .ok_or_else(|| format!("expected float probe value, received {value:?}")),
    }
}

impl GpuPrototype {
    fn new(allow_software_adapter: bool) -> Result<Self, String> {
        let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(instance_descriptor);
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
        let adapter = adapters
            .into_iter()
            .find(|adapter| {
                allow_software_adapter
                    || !matches!(adapter.get_info().device_type, wgpu::DeviceType::Cpu)
            })
            .ok_or_else(|| {
                if allow_software_adapter {
                    "no Vulkan adapter is available".to_owned()
                } else {
                    "no hardware Vulkan adapter is available; pass --allow-software-adapter to use a CPU adapter"
                        .to_owned()
                }
            })?;
        let adapter_info = adapter.get_info();
        let supported = adapter.features();
        let required = wgpu::Features::SHADER_INT64 | wgpu::Features::TIMESTAMP_QUERY;
        if !supported.contains(required) {
            return Err(format!(
                "adapter {:?} lacks required features {:?}",
                adapter_info.name,
                required - supported
            ));
        }
        let shared_mappable = supported.contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS);
        let requested_features =
            required | supported.intersection(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("mica-wgpu-membership-device"),
            required_features: requested_features,
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|error| format!("failed to request Vulkan device: {error}"))?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mica-wgpu-membership-shader"),
            source: wgpu::ShaderSource::Wgsl(MEMBERSHIP_SHADER.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("mica-wgpu-membership-pipeline"),
            layout: None,
            module: &shader,
            entry_point: Some("membership"),
            compilation_options: Default::default(),
            cache: None,
        });
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("mica-wgpu-membership-timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let timestamp_resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mica-wgpu-membership-timestamp-resolve"),
            size: wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let timestamp_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mica-wgpu-membership-timestamp-readback"),
            size: TIMESTAMP_BYTES,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            adapter_info,
            device,
            queue,
            pipeline,
            query_set,
            timestamp_resolve,
            timestamp_readback,
            shared_mappable,
        })
    }

    fn supported_modes(&self) -> impl Iterator<Item = GpuBufferMode> {
        [
            Some(GpuBufferMode::StagedReadback),
            self.shared_mappable
                .then_some(GpuBufferMode::SharedMappable),
        ]
        .into_iter()
        .flatten()
    }

    fn benchmark_cold(
        &self,
        mode: GpuBufferMode,
        left: &[Tuple],
        right: &[Tuple],
        expected: &[Tuple],
        domain: ProbeDomain,
        iterations: usize,
    ) -> Result<Vec<GpuSample>, String> {
        let mut samples = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let started = Instant::now();
            let packed = pack_gpu_inputs(left, right, domain)?;
            let buffers = self.create_buffers(mode, &packed.left, &packed.right);
            let mut sample = self.execute(&buffers, left)?;
            sample.wall = started.elapsed();
            sample.columnize = packed.columnize;
            sample.encode = packed.encode;
            verify_gpu_rows(&sample.rows, expected)?;
            samples.push(sample);
        }
        Ok(samples)
    }

    fn benchmark_warm(
        &self,
        mode: GpuBufferMode,
        left: &[Tuple],
        right: &[Tuple],
        expected: &[Tuple],
        domain: ProbeDomain,
        iterations: WarmIterations,
    ) -> Result<Vec<GpuSample>, String> {
        let packed = pack_gpu_inputs(left, right, domain)?;
        let buffers = self.create_buffers(mode, &packed.left, &packed.right);
        for _ in 0..iterations.warmup {
            let sample = self.execute(&buffers, left)?;
            verify_gpu_rows(&sample.rows, expected)?;
        }
        let mut samples = Vec::with_capacity(iterations.measured);
        for _ in 0..iterations.measured {
            let mut sample = self.execute(&buffers, left)?;
            sample.columnize = packed.columnize;
            sample.encode = packed.encode;
            verify_gpu_rows(&sample.rows, expected)?;
            samples.push(sample);
        }
        Ok(samples)
    }

    fn create_buffers(&self, mode: GpuBufferMode, left: &[u64], right: &[u64]) -> OperatorBuffers {
        let input_usage = match mode {
            GpuBufferMode::StagedReadback => wgpu::BufferUsages::STORAGE,
            GpuBufferMode::SharedMappable => {
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::MAP_WRITE
            }
        };
        let left_buffer = create_u64_buffer(&self.device, "left", left, input_usage);
        let right_buffer = create_u64_buffer(&self.device, "right", right, input_usage);
        let output_size = (left.len() * size_of::<u32>()) as u64;
        let output_usage = match mode {
            GpuBufferMode::StagedReadback => {
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC
            }
            GpuBufferMode::SharedMappable => {
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::MAP_READ
            }
        };
        let output = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mica-wgpu-membership-output"),
            size: output_size,
            usage: output_usage,
            mapped_at_creation: false,
        });
        let readback = (mode == GpuBufferMode::StagedReadback).then(|| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mica-wgpu-membership-output-readback"),
                size: output_size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let params = [left.len() as u32, right.len() as u32, 0, 0];
        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mica-wgpu-membership-params"),
                contents: u32_bytes(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let layout = self.pipeline.get_bind_group_layout(0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mica-wgpu-membership-bind-group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: left_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: right_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params_buffer.as_entire_binding(),
                },
            ],
        });

        OperatorBuffers {
            bind_group,
            output,
            readback,
            row_count: left.len(),
        }
    }

    fn execute(&self, buffers: &OperatorBuffers, left: &[Tuple]) -> Result<GpuSample, String> {
        let started = Instant::now();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mica-wgpu-membership-command-encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mica-wgpu-membership-compute-pass"),
                timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                    query_set: &self.query_set,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(1),
                }),
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &buffers.bind_group, &[]);
            pass.dispatch_workgroups((buffers.row_count as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        let output_readback = buffers.readback.as_ref().unwrap_or(&buffers.output);
        if let Some(readback) = &buffers.readback {
            encoder.copy_buffer_to_buffer(&buffers.output, 0, readback, 0, buffers.output.size());
        }
        encoder.resolve_query_set(&self.query_set, 0..2, &self.timestamp_resolve, 0);
        encoder.copy_buffer_to_buffer(
            &self.timestamp_resolve,
            0,
            &self.timestamp_readback,
            0,
            TIMESTAMP_BYTES,
        );
        self.queue.submit([encoder.finish()]);

        let output_slice = output_readback.slice(..);
        let timestamp_slice = self.timestamp_readback.slice(..TIMESTAMP_BYTES);
        let output_result = map_for_read(&output_slice);
        let timestamp_result = map_for_read(&timestamp_slice);
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| format!("failed while waiting for Vulkan work: {error}"))?;
        output_result
            .recv()
            .map_err(|_| "output mapping callback was dropped".to_owned())?
            .map_err(|error| format!("failed to map membership output: {error}"))?;
        timestamp_result
            .recv()
            .map_err(|_| "timestamp mapping callback was dropped".to_owned())?
            .map_err(|error| format!("failed to map timestamps: {error}"))?;

        let output_view = output_slice
            .get_mapped_range()
            .map_err(|error| format!("failed to access mapped membership output: {error}"))?;
        let rows = materialize_matches(left, &output_view);
        drop(output_view);
        output_readback.unmap();
        let timestamp_view = timestamp_slice
            .get_mapped_range()
            .map_err(|error| format!("failed to access mapped timestamps: {error}"))?;
        let start_tick = read_u64(&timestamp_view[..8]);
        let end_tick = read_u64(&timestamp_view[8..16]);
        drop(timestamp_view);
        self.timestamp_readback.unmap();
        let kernel_ns =
            (end_tick - start_tick) as f64 * f64::from(self.queue.get_timestamp_period());

        Ok(GpuSample {
            wall: started.elapsed(),
            kernel: Duration::from_nanos(kernel_ns.round() as u64),
            columnize: Duration::ZERO,
            encode: Duration::ZERO,
            rows,
        })
    }
}

fn create_u64_buffer(
    device: &wgpu::Device,
    label: &str,
    values: &[u64],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: std::mem::size_of_val(values) as u64,
        usage,
        mapped_at_creation: true,
    });
    buffer
        .slice(..)
        .get_mapped_range_mut()
        .expect("newly created input buffer should remain mapped")
        .copy_from_slice(u64_bytes(values));
    buffer.unmap();
    buffer
}

fn map_for_read(
    slice: &wgpu::BufferSlice<'_>,
) -> mpsc::Receiver<Result<(), wgpu::BufferAsyncError>> {
    let (sender, receiver) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    receiver
}

fn materialize_matches(left: &[Tuple], bytes: &[u8]) -> Vec<Tuple> {
    left.iter()
        .zip(bytes.as_chunks::<{ size_of::<u32>() }>().0)
        .filter(|(_, flag)| read_u32(*flag) != 0)
        .map(|(tuple, _)| tuple.clone())
        .collect()
}

fn verify_gpu_rows(actual: &[Tuple], expected: &[Tuple]) -> Result<(), String> {
    if actual == expected {
        return Ok(());
    }
    Err(format!(
        "GPU result mismatch: expected {} rows, received {}",
        expected.len(),
        actual.len()
    ))
}

fn print_cpu_result(shape: WorkloadShape, backend: &str, duration: Duration, serial: Duration) {
    println!(
        "{},{},{},{},{},{backend},resident,,,{:.3},,{:.3}",
        shape.value_domain.label(),
        shape.probe_order.label(),
        shape.match_rate.label(),
        shape.rows,
        shape.hits,
        duration.as_secs_f64() * 1_000_000.0,
        serial.as_secs_f64() / duration.as_secs_f64(),
    );
}

fn print_gpu_result(
    shape: WorkloadShape,
    mode: GpuBufferMode,
    residency: &str,
    samples: &[GpuSample],
    serial: Duration,
) {
    let mut wall = samples.iter().map(|sample| sample.wall).collect::<Vec<_>>();
    let mut kernel = samples
        .iter()
        .map(|sample| sample.kernel)
        .collect::<Vec<_>>();
    let mut columnize = samples
        .iter()
        .map(|sample| sample.columnize)
        .collect::<Vec<_>>();
    let mut encode = samples
        .iter()
        .map(|sample| sample.encode)
        .collect::<Vec<_>>();
    let wall = median_duration(&mut wall);
    let kernel = median_duration(&mut kernel);
    let columnize = median_duration(&mut columnize);
    let encode = median_duration(&mut encode);
    println!(
        "{},{},{},{},{},{},{residency},{:.3},{:.3},{:.3},{:.3},{:.3}",
        shape.value_domain.label(),
        shape.probe_order.label(),
        shape.match_rate.label(),
        shape.rows,
        shape.hits,
        mode.label(),
        columnize.as_secs_f64() * 1_000_000.0,
        encode.as_secs_f64() * 1_000_000.0,
        wall.as_secs_f64() * 1_000_000.0,
        kernel.as_secs_f64() * 1_000_000.0,
        serial.as_secs_f64() / wall.as_secs_f64(),
    );
}

fn median_duration(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_ne_bytes(bytes.try_into().unwrap())
}

fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_ne_bytes(bytes.try_into().unwrap())
}

fn u32_bytes(values: &[u32]) -> &[u8] {
    // SAFETY: Every `u32` byte pattern is valid, and the returned slice has the
    // same lifetime and exact byte extent as `values`.
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

fn u64_bytes(values: &[u64]) -> &[u8] {
    // SAFETY: Every `u64` byte pattern is valid, and the returned slice has the
    // same lifetime and exact byte extent as `values`.
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_inputs_have_requested_match_rates() {
        for (match_rate, expected) in [
            (MatchRate::None, 0),
            (MatchRate::Half, 512),
            (MatchRate::All, 1_024),
        ] {
            let (left, right) =
                make_inputs(1_024, ProbeOrder::Sequential, match_rate, ProbeDomain::Int);
            assert_eq!(membership_serial(&left, &right).len(), expected);
        }
    }

    #[test]
    fn mixed_inputs_preserve_requested_all_or_none_semantics() {
        for (match_rate, expected) in [(MatchRate::None, 0), (MatchRate::All, 1_024)] {
            let (left, right) =
                make_inputs(1_024, ProbeOrder::Mixed, match_rate, ProbeDomain::Identity);
            assert_eq!(membership_serial(&left, &right).len(), expected);
        }
    }

    #[test]
    fn rayon_membership_matches_serial_membership() {
        let (left, right) = make_inputs(
            16_384,
            ProbeOrder::Mixed,
            MatchRate::Half,
            ProbeDomain::Float,
        );
        assert_eq!(
            membership_rayon(&left, &right),
            membership_serial(&left, &right)
        );
    }

    #[test]
    fn fixed_width_encodings_preserve_value_order() {
        let cases = [
            (
                ProbeDomain::Int,
                vec![
                    Value::int(-7).unwrap(),
                    Value::int(0).unwrap(),
                    Value::int(9).unwrap(),
                ],
            ),
            (
                ProbeDomain::Identity,
                vec![
                    Value::identity(Identity::new(0).unwrap()),
                    Value::identity(Identity::new(3).unwrap()),
                    Value::identity(Identity::new(10).unwrap()),
                ],
            ),
            (
                ProbeDomain::Float,
                vec![
                    Value::float(-7.5).unwrap(),
                    Value::float(0.0).unwrap(),
                    Value::float(9.25).unwrap(),
                ],
            ),
        ];

        for (domain, values) in cases {
            assert!(values.windows(2).all(|window| window[0] < window[1]));
            let encoded = encode_column(&values, domain).unwrap();
            assert!(encoded.windows(2).all(|window| window[0] < window[1]));
        }
    }

    #[test]
    fn encoding_rejects_a_mismatched_value_domain() {
        let error = encode_value(&Value::float(1.0).unwrap(), ProbeDomain::Int).unwrap_err();
        assert!(error.contains("expected int probe value"));
    }

    #[test]
    fn packed_gpu_inputs_start_from_real_tuple_columns() {
        let (left, right) =
            make_inputs(1_024, ProbeOrder::Mixed, MatchRate::Half, ProbeDomain::Int);
        let packed = pack_gpu_inputs(&left, &right, ProbeDomain::Int).unwrap();
        assert_eq!(packed.left.len(), left.len());
        assert_eq!(packed.right.len(), right.len());
    }
}
