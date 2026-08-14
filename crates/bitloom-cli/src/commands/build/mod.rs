use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::sync::Arc;

use anyhow::{Context, Result};
use bitloom_build::{BuildPipeline, BuildTasks, DesignPipeline, DesignSpec};
use bitloom_cache::{CacheParameters, PackCache, PnrCache, SnapshotCache, SynthesisCache};
use bitloom_config::ResolvedProject;
use bitloom_gowin::{
    GowinDevice, GowinPackBackend, GowinPnrBackend, GowinSynthesisBackend, RoutedNetlist, Toolchain,
};
use bitloom_pnr::{PnrConfig, PnrStatistics};
use bitloom_snapshot::{SnapshotInput, SnapshotTask};

use crate::cli::BuildArgs;
use crate::{execution, output, project};

const PNR_RUNS: u64 = 64;
const PNR_PARALLELISM: usize = 8;

pub(super) type GowinBuildTasks =
    BuildTasks<GowinSynthesisBackend, GowinPnrBackend, GowinPackBackend>;
pub(super) type GowinDesignPipeline =
    DesignPipeline<GowinSynthesisBackend, GowinPnrBackend, GowinPackBackend>;

pub async fn run(args: BuildArgs) -> Result<()> {
    let project = project::load_resolved().await?;
    project::clear_build_outputs(&project.root).await?;
    let toolchain = crate::logging::attach(
        project::resolve_toolchain(&project.root, &project.config)?,
        &project.root,
        args.output,
    )?;
    toolchain.refresh_gdk_cache().await?;
    let pipeline: BuildPipeline<_, _, _> = create_tasks(&project, toolchain).await?.build()?;
    execution::run(&pipeline.graph, args.output).await?;
    if args.stat {
        print_statistics(&pipeline.designs).await?;
    }
    Ok(())
}

pub(super) async fn create_tasks(
    project: &ResolvedProject,
    toolchain: Toolchain,
) -> Result<GowinBuildTasks> {
    let device =
        GowinDevice::resolve(&project.config.target.vendor, &project.config.target.device)?;
    let snapshot_root = project.root.join("build/snapshot");
    let mut inputs = project.sources.iter().cloned().map(SnapshotInput::source).collect::<Vec<_>>();
    let mut seen_constraints = BTreeSet::new();
    for constraints in &project.constraints {
        for path in constraints {
            if seen_constraints.insert(path.clone()) {
                inputs.push(SnapshotInput::constraint(path.clone()));
            }
        }
    }
    let cache_root = project.root.join("build/cache");
    let toolchain_fingerprint = toolchain.cache_fingerprint().await?;
    let snapshot = SnapshotTask::new(&project.root, &snapshot_root, inputs)?;
    let snapshot_cache = SnapshotCache::new(
        &cache_root,
        snapshot.project_root(),
        snapshot.output_root(),
        snapshot.inputs(),
    )
    .await?;
    let snapshot = snapshot.with_cache(Arc::new(snapshot_cache));
    let parallelism =
        NonZeroUsize::new(PNR_PARALLELISM).context("PNR parallelism must be greater than zero")?;
    let seeds = (0..PNR_RUNS).collect::<Vec<_>>();
    let mut designs = Vec::with_capacity(project.config.bitstreams.len());
    for (bitstream, constraints) in project.config.bitstreams.iter().zip(project.constraints.iter())
    {
        let design_toolchain = toolchain.clone().with_process_scope(bitstream.name.as_str());
        let design_root = project.root.join("build/designs").join(&bitstream.name);
        let synthesis_output = design_root.join("synthesis.json");
        let pnr_root = design_root.join("pnr");
        let bitstream_output = project::bitstream_path(&project.root, &bitstream.name);
        let snapshot_constraints =
            constraints.iter().map(|path| snapshot_root.join(path)).collect::<Vec<_>>();

        let mut synthesis_parameters = CacheParameters::new("gowin-synthesis");
        synthesis_parameters
            .add_fingerprint("toolchain", toolchain_fingerprint)
            .add_str("top", &bitstream.top)
            .add_str("family", device.yosys_family);

        let mut pnr_parameters = CacheParameters::new("gowin-pnr");
        pnr_parameters
            .add_fingerprint("toolchain", toolchain_fingerprint)
            .add_str("top", &bitstream.top)
            .add_str("device", &project.config.target.device)
            .add_str("family", device.nextpnr_family)
            .add_f64("frequency_mhz", bitstream.frequency_mhz)
            .add_usize("parallelism", parallelism.get());
        for seed in &seeds {
            pnr_parameters.add_u64("seed", *seed);
        }
        for constraint in constraints {
            pnr_parameters.add_file("constraint", &project.root, constraint).await?;
        }

        let mut pack_parameters = CacheParameters::new("gowin-pack");
        pack_parameters
            .add_fingerprint("toolchain", toolchain_fingerprint)
            .add_str("device", device.nextpnr_family);

        let specification = DesignSpec::new(
            bitstream.name.as_str(),
            GowinSynthesisBackend::with_toolchain(
                design_toolchain.clone(),
                bitstream.top.as_str(),
                device.yosys_family,
                &synthesis_output,
            ),
            GowinPnrBackend::with_toolchain(
                design_toolchain.clone(),
                bitstream.top.as_str(),
                project.config.target.device.as_str(),
                device.nextpnr_family,
                bitstream.frequency_mhz,
                snapshot_constraints,
                &pnr_root,
            ),
            PnrConfig::new(seeds.iter().copied(), parallelism)?,
            GowinPackBackend::with_toolchain(
                design_toolchain,
                device.nextpnr_family,
                &bitstream_output,
            ),
        )
        .with_caches(
            Arc::new(SynthesisCache::new(&cache_root, &synthesis_output, synthesis_parameters)),
            Arc::new(PnrCache::<RoutedNetlist>::new(
                &cache_root,
                pnr_root.join("cached-selected.json"),
                pnr_parameters,
            )),
            Arc::new(PackCache::new(&cache_root, &bitstream_output, pack_parameters)),
        );
        designs.push(specification);
    }
    Ok(BuildTasks::new(snapshot, designs)?)
}

pub(super) async fn print_statistics(designs: &[GowinDesignPipeline]) -> Result<()> {
    for design in designs {
        let output = design.pnr.result().await?;
        output::line(render_statistics(&design.name, &output.selected.statistics))?;
    }
    Ok(())
}

fn render_statistics(name: &str, statistics: &PnrStatistics) -> String {
    const PRIMARY_RESOURCES: [&str; 9] =
        ["LUT4", "DFF", "ALU", "BSRAM", "MULT18X18", "MULTALU18X18", "IOB", "BUFG", "rPLL"];
    const HIDDEN_RESOURCES: [&str; 3] = ["GND", "VCC", "GSR"];

    let mut output = format!(
        "STATISTICS ({name})\n{:<18} {:>10} {:>12} {:>12}",
        "RESOURCE", "USED", "AVAILABLE", "UTILIZATION"
    );
    let mut resource_names = PRIMARY_RESOURCES
        .into_iter()
        .filter(|resource| statistics.resources.contains_key(*resource))
        .collect::<Vec<_>>();
    for (resource, usage) in &statistics.resources {
        if usage.used > 0
            && !HIDDEN_RESOURCES.contains(&resource.as_str())
            && !resource_names.contains(&resource.as_str())
        {
            resource_names.push(resource);
        }
    }
    for resource in resource_names {
        let usage = statistics.resources[resource];
        let utilization = if usage.available == 0 {
            0.0
        } else {
            usage.used as f64 / usage.available as f64 * 100.0
        };
        output.push_str(&format!(
            "\n{resource:<18} {:>10} {:>12} {:>11.2}%",
            usage.used, usage.available, utilization
        ));
    }

    output
        .push_str(&format!("\n\n{:<24} {:>12} {:>12} {:>12}", "CLOCK", "FMAX", "TARGET", "MARGIN"));
    if statistics.clocks.is_empty() {
        output.push_str("\n(no constrained clocks)");
    } else {
        for (clock, frequency) in &statistics.clocks {
            output.push_str(&format!(
                "\n{clock:<24} {:>8.2} MHz {:>8.2} MHz {:+8.2} MHz",
                frequency.achieved_mhz,
                frequency.constraint_mhz,
                frequency.achieved_mhz - frequency.constraint_mhz,
            ));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_selected_pnr_statistics() {
        let statistics = PnrStatistics {
            resources: std::collections::BTreeMap::from([
                ("LUT4".to_owned(), bitloom_pnr::ResourceUsage { used: 9, available: 20_736 }),
                ("DFF".to_owned(), bitloom_pnr::ResourceUsage { used: 24, available: 15_552 }),
            ]),
            clocks: std::collections::BTreeMap::from([(
                "clk".to_owned(),
                bitloom_pnr::ClockFrequency { achieved_mhz: 446.23, constraint_mhz: 27.0 },
            )]),
        };

        let rendered = render_statistics("top", &statistics);

        assert!(rendered.contains("STATISTICS (top)"));
        assert!(rendered.contains("LUT4"));
        assert!(rendered.contains("20736"));
        assert!(rendered.contains("446.23 MHz"));
        assert!(rendered.contains("+419.23 MHz"));
    }
}
