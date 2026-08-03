//! Benchmark suite for bt_iden identity resolution.
//!
//! Uses Criterion to measure performance characteristics including:
//! - Observations per second
//! - Memory allocations
//! - Latency under various workloads

use bt_iden::models::{AddressType, AdvertisementObservation, BluetoothAddress};
use bt_iden::{HeuristicIdentityResolver, IdentityResolver, ResolverConfig};
use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use std::time::{Duration, Instant};

/// Generate a deterministic test address from an index.
fn gen_addr(index: usize) -> BluetoothAddress {
    BluetoothAddress::new([
        0x00,
        0x00,
        ((index >> 16) & 0xFF) as u8,
        ((index >> 8) & 0xFF) as u8,
        (index & 0xFF) as u8,
        ((index * 7) & 0xFF) as u8,
    ])
}

/// Generate an observation with some realistic characteristics.
fn gen_obs(ts: Instant, addr: BluetoothAddress, index: usize) -> AdvertisementObservation {
    AdvertisementObservation::new(ts, addr, AddressType::PrivateResolvable)
        .with_rssi(-65 + (index % 20) as i16)
        .with_manufacturer_data(0x004C + (index % 10) as u16, vec![0x01, 0x02, 0x03])
}

/// Benchmark single device, same address repeatedly.
fn bench_single_device_same_address(c: &mut Criterion) {
    let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
    let addr = gen_addr(42);
    let t = Instant::now();

    let mut group = c.benchmark_group("single_device/same_address");
    group.throughput(Throughput::Elements(1000));

    group.bench_function("1000_observations", |b| {
        b.iter(|| {
            resolver.reset();
            for i in 0..1000 {
                let obs = gen_obs(t + Duration::from_micros(i as u64), addr, 42);
                black_box(resolver.observe(obs));
            }
        });
    });
}

/// Benchmark single device with address rotation.
fn bench_single_device_address_rotation(c: &mut Criterion) {
    let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
    let t = Instant::now();

    let mut group = c.benchmark_group("single_device/address_rotation");
    group.throughput(Throughput::Elements(1000));

    group.bench_function("1000_observations_rotating", |b| {
        b.iter(|| {
            resolver.reset();
            for i in 0..1000 {
                let addr = gen_addr((42 + i * 7) % 256); // Rotate through addresses
                let obs = gen_obs(t + Duration::from_micros(i as u64), addr, 42);
                black_box(resolver.observe(obs));
            }
        });
    });
}

/// Benchmark many devices, each with unique identity.
fn bench_many_devices_unique(c: &mut Criterion) {
    let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
    let t = Instant::now();

    let mut group = c.benchmark_group("many_devices/unique");
    group.throughput(Throughput::Elements(100));

    group.bench_function("100_devices", |b| {
        b.iter(|| {
            resolver.reset();
            for i in 0..100 {
                let addr = gen_addr(i);
                let obs = gen_obs(t, addr, i);
                black_box(resolver.observe(obs));
            }
        });
    });

    group.throughput(Throughput::Elements(500));

    group.bench_function("500_devices", |b| {
        b.iter(|| {
            resolver.reset();
            for i in 0..500 {
                let addr = gen_addr(i);
                let obs = gen_obs(t, addr, i);
                black_box(resolver.observe(obs));
            }
        });
    });

    group.throughput(Throughput::Elements(1000));

    group.bench_function("1000_devices", |b| {
        b.iter(|| {
            resolver.reset();
            for i in 0..1000 {
                let addr = gen_addr(i);
                let obs = gen_obs(t, addr, i);
                black_box(resolver.observe(obs));
            }
        });
    });
}

/// Benchmark address rotation for many devices.
fn bench_many_devices_with_rotation(c: &mut Criterion) {
    let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
    let t = Instant::now();

    let num_devices = 100;
    let rotations_per_device = 10;
    let total_obs = num_devices * rotations_per_device;

    let mut group = c.benchmark_group("many_devices/with_rotation");
    group.throughput(Throughput::Elements(total_obs as u64));

    group.bench_function("100_devices_10_rotations_each", |b| {
        b.iter(|| {
            resolver.reset();
            for device_idx in 0..num_devices {
                for rotation in 0..rotations_per_device {
                    let addr = gen_addr(device_idx * rotations_per_device + rotation);
                    let obs = gen_obs(
                        t + Duration::from_micros(
                            (device_idx * rotations_per_device + rotation) as u64,
                        ),
                        addr,
                        device_idx,
                    );
                    black_box(resolver.observe(obs));
                }
            }
        });
    });
}

/// Benchmark expiration overhead.
fn bench_expiration(c: &mut Criterion) {
    let config = ResolverConfig::builder()
        .max_identity_age(Duration::from_secs(5))
        .build();
    let mut resolver = HeuristicIdentityResolver::new(config);
    let t = Instant::now();

    // Populate with 100 devices
    for i in 0..100 {
        let addr = gen_addr(i);
        let obs = gen_obs(t, addr, i);
        resolver.observe(obs);
    }

    let mut group = c.benchmark_group("expiration");
    group.throughput(Throughput::Elements(100));

    group.bench_function("expire_100_devices", |b| {
        b.iter(|| {
            resolver.expire(t + Duration::from_secs(10));
        });
    });
}

/// Benchmark reset operation.
fn bench_reset(c: &mut Criterion) {
    let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
    let t = Instant::now();

    // Populate with 100 devices
    for i in 0..100 {
        let addr = gen_addr(i);
        let obs = gen_obs(t, addr, i);
        resolver.observe(obs);
    }

    let mut group = c.benchmark_group("reset");
    group.throughput(Throughput::Elements(100));

    group.bench_function("reset_100_identities", |b| {
        b.iter(|| {
            resolver.reset();
        });
    });
}

/// Benchmark with varying observation complexity.
fn bench_complex_observations(c: &mut Criterion) {
    use uuid::Uuid;

    let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
    let t = Instant::now();

    // Create complex observations with multiple UUIDs and service data
    let mut group = c.benchmark_group("complex_observations");
    group.throughput(Throughput::Elements(100));

    group.bench_function("100_complex_observations", |b| {
        b.iter(|| {
            resolver.reset();
            for i in 0..100 {
                let addr = gen_addr(i);
                let mut obs =
                    AdvertisementObservation::new(t, addr, AddressType::PrivateResolvable)
                        .with_rssi(-65)
                        .with_manufacturer_data(0x004C, vec![0x01, 0x02, 0x03, 0x04, 0x05])
                        .with_local_name(format!("Device{}", i % 10));

                // Add multiple service UUIDs
                for _ in 0..5 {
                    let uuid = Uuid::new_v4();
                    obs = obs.with_service_uuid(uuid);
                }

                black_box(resolver.observe(obs));
            }
        });
    });
}

/// Benchmark memory allocation patterns.
fn bench_allocation_patterns(c: &mut Criterion) {
    let mut resolver = HeuristicIdentityResolver::new(ResolverConfig::default());
    let t = Instant::now();

    let mut group = c.benchmark_group("allocations");
    group.throughput(Throughput::Elements(1000));

    group.bench_function("1000_simple_obs", |b| {
        b.iter(|| {
            resolver.reset();
            for i in 0..1000 {
                let addr = gen_addr(i % 100); // Reuse addresses to trigger merges
                let obs = AdvertisementObservation::new(
                    t + Duration::from_micros(i as u64),
                    addr,
                    AddressType::PrivateResolvable,
                )
                .with_rssi(-65);
                black_box(resolver.observe(obs));
            }
        });
    });
}

fn benchmark_main(c: &mut Criterion) {
    bench_single_device_same_address(c);
    bench_single_device_address_rotation(c);
    bench_many_devices_unique(c);
    bench_many_devices_with_rotation(c);
    bench_expiration(c);
    bench_reset(c);
    bench_complex_observations(c);
    bench_allocation_patterns(c);
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(10))
        .sample_size(100);
    targets = benchmark_main
);

criterion_main!(benches);
