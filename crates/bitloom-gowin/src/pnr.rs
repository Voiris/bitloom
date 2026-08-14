use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bitloom_artifacts::{Artifact, ArtifactKind};
use bitloom_core::{OutputFingerprint, TaskContext};
use bitloom_pnr::{
    ClockFrequency, PnrBackend, PnrCandidate, PnrFailureKind, PnrRun, PnrRunContext, PnrScore,
    PnrStatistics, ResourceUsage,
};
use bitloom_synthesis::SynthesisOutput;
use serde::Deserialize;

use crate::filesystem::{RemoveFile, ensure_output, fingerprint_file};
use crate::process::{run_tool_cancellable, tool_path};
use crate::{ToolError, Toolchain};

#[derive(Debug)]
pub struct RoutedNetlist;

impl ArtifactKind for RoutedNetlist {}

pub struct GowinPnrBackend {
    toolchain: Toolchain,
    top: Arc<str>,
    device: Arc<str>,
    family: &'static str,
    frequency_mhz: f64,
    constraints: Arc<[PathBuf]>,
    output_dir: PathBuf,
}

impl GowinPnrBackend {
    pub fn new(
        top: impl Into<Arc<str>>,
        device: impl Into<Arc<str>>,
        family: &'static str,
        frequency_mhz: f64,
        constraints: impl IntoIterator<Item = PathBuf>,
        output_dir: impl Into<PathBuf>,
    ) -> Self {
        Self::with_toolchain(
            Toolchain::default(),
            top,
            device,
            family,
            frequency_mhz,
            constraints,
            output_dir,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_toolchain(
        toolchain: Toolchain,
        top: impl Into<Arc<str>>,
        device: impl Into<Arc<str>>,
        family: &'static str,
        frequency_mhz: f64,
        constraints: impl IntoIterator<Item = PathBuf>,
        output_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            toolchain,
            top: top.into(),
            device: device.into(),
            family,
            frequency_mhz,
            constraints: constraints.into_iter().collect(),
            output_dir: output_dir.into(),
        }
    }
}

impl PnrBackend<SynthesisOutput> for GowinPnrBackend {
    type Artifact = Artifact<RoutedNetlist>;
    type Error = ToolError;

    async fn prepare(
        &self,
        context: TaskContext,
        _input: Arc<SynthesisOutput>,
    ) -> Result<(), Self::Error> {
        tokio::fs::create_dir_all(&self.output_dir)
            .await
            .map_err(|source| ToolError::Prepare { path: self.output_dir.clone(), source })?;

        if !self.constraints.is_empty() {
            let constraints = self.output_dir.join("constraints.cst");
            context.defer(RemoveFile::new(constraints.clone()));
            combine_constraints(&self.constraints, &constraints).await?;
        }
        Ok(())
    }

    async fn run(
        &self,
        context: PnrRunContext,
        input: Arc<SynthesisOutput>,
        seed: u64,
    ) -> Result<PnrRun<Self::Artifact>, Self::Error> {
        let output = self.output_dir.join(format!("candidate-{seed}.json"));
        let report = self.output_dir.join(format!("report-{seed}.json"));
        context.defer(RemoveFile::new(output.clone()));
        context.defer(RemoveFile::new(report.clone()));

        let mut command = self.toolchain.command("nextpnr-himbaechel");
        command
            .arg("--device")
            .arg(self.device.as_ref())
            .arg("--vopt")
            .arg(format!("family={}", self.family));
        if !self.constraints.is_empty() {
            let constraints = self.output_dir.join("constraints.cst");
            command.arg("--vopt").arg(format!("cst={}", tool_path(&constraints)));
        }
        command
            .arg("--json")
            .arg(input.netlist.path())
            .arg("--write")
            .arg(&output)
            .arg("--report")
            .arg(&report)
            .arg("--top")
            .arg(self.top.as_ref())
            .arg("--freq")
            .arg(self.frequency_mhz.to_string())
            .arg("--seed")
            .arg(seed.to_string());
        run_tool_cancellable(
            command,
            "nextpnr",
            context.cancellation(),
            self.toolchain.process_scope(),
            self.toolchain.process_observer(),
        )
        .await?;
        ensure_output("nextpnr", &output).await?;
        ensure_output("nextpnr", &report).await?;
        let (score, statistics) = report_summary(&report).await?;
        let fingerprint = fingerprint_file(&output).await?;
        Ok(PnrRun {
            score,
            artifact: Artifact::<RoutedNetlist>::new(output, fingerprint),
            statistics,
        })
    }

    fn failure_kind(&self, error: &Self::Error) -> PnrFailureKind {
        match error {
            ToolError::Exit { stderr, .. } if retryable_nextpnr_failure(stderr) => {
                PnrFailureKind::Retryable
            }
            _ => PnrFailureKind::Fatal,
        }
    }

    fn format_error(&self, error: &Self::Error) -> String {
        match error {
            ToolError::Exit { tool, status, stderr } => {
                format!("{tool} failed with {status}: {}", tool_output_diagnostic(stderr))
            }
            _ => error.to_string(),
        }
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        vec![self.output_dir.clone()]
    }

    fn output_fingerprint(
        &self,
        candidate: &PnrCandidate<Self::Artifact>,
    ) -> Option<OutputFingerprint> {
        Some(candidate.artifact.fingerprint())
    }
}

fn retryable_nextpnr_failure(stderr: &str) -> bool {
    let diagnostic = tool_output_diagnostic(stderr);
    let diagnostic = diagnostic.strip_prefix("ERROR: ").unwrap_or(diagnostic);
    diagnostic == "Placing design failed."
        || diagnostic == "Routing design failed."
        || diagnostic.starts_with("Failed to route arc ")
}

fn tool_output_diagnostic(stderr: &str) -> &str {
    let mut fallback = None;
    for line in stderr.lines().rev().map(str::trim).filter(|line| !line.is_empty()) {
        fallback.get_or_insert(line);
        if line.starts_with("ERROR: ") || line.starts_with("FATAL: ") {
            return line;
        }
    }
    fallback.unwrap_or("no diagnostic output")
}

#[derive(Debug, Deserialize)]
struct TimingReport {
    #[serde(default)]
    fmax: BTreeMap<String, ClockTiming>,
    #[serde(default)]
    utilization: BTreeMap<String, ToolResourceUsage>,
}

#[derive(Debug, Deserialize)]
struct ClockTiming {
    achieved: f64,
    constraint: f64,
}

#[derive(Debug, Deserialize)]
struct ToolResourceUsage {
    used: u64,
    available: u64,
}

async fn report_summary(path: &Path) -> Result<(PnrScore, PnrStatistics), ToolError> {
    let source = tokio::fs::read(path)
        .await
        .map_err(|source| ToolError::Read { path: path.to_owned(), source })?;
    let report: TimingReport = serde_json::from_slice(&source)
        .map_err(|source| ToolError::Report { path: path.to_owned(), source })?;
    summarize_report(report)
}

fn summarize_report(report: TimingReport) -> Result<(PnrScore, PnrStatistics), ToolError> {
    let score = report
        .fmax
        .values()
        .map(|clock| clock.achieved - clock.constraint)
        .reduce(f64::min)
        .unwrap_or(0.0);
    let score = PnrScore::new(score).map_err(|_| ToolError::InvalidScore)?;
    let clocks = report
        .fmax
        .into_iter()
        .map(|(name, clock)| {
            (
                name,
                ClockFrequency { achieved_mhz: clock.achieved, constraint_mhz: clock.constraint },
            )
        })
        .collect();
    let resources = report
        .utilization
        .into_iter()
        .map(|(name, usage)| (name, ResourceUsage { used: usage.used, available: usage.available }))
        .collect();
    Ok((score, PnrStatistics { resources, clocks }))
}

async fn combine_constraints(inputs: &[PathBuf], output: &Path) -> Result<(), ToolError> {
    if inputs.is_empty() {
        return Ok(());
    }
    let mut combined = Vec::new();
    for input in inputs {
        let bytes = tokio::fs::read(input)
            .await
            .map_err(|source| ToolError::Read { path: input.clone(), source })?;
        combined.extend_from_slice(&bytes);
        if !combined.ends_with(b"\n") {
            combined.push(b'\n');
        }
    }
    tokio::fs::write(output, combined)
        .await
        .map_err(|source| ToolError::Prepare { path: output.to_owned(), source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_utilization_and_clock_frequencies() {
        let report: TimingReport = serde_json::from_str(
            r#"{
                "fmax": {"clk": {"achieved": 123.5, "constraint": 27.0}},
                "utilization": {"LUT4": {"used": 42, "available": 20736}}
            }"#,
        )
        .unwrap();

        let (score, statistics) = summarize_report(report).unwrap();

        assert_eq!(score.get(), 96.5);
        assert_eq!(statistics.resources["LUT4"], ResourceUsage { used: 42, available: 20736 });
        assert_eq!(
            statistics.clocks["clk"],
            ClockFrequency { achieved_mhz: 123.5, constraint_mhz: 27.0 }
        );
    }

    #[test]
    fn extracts_the_last_tool_error_from_verbose_output() {
        let stderr = "Info: packing\nERROR: invalid constraint on pin 3\n";

        assert_eq!(tool_output_diagnostic(stderr), "ERROR: invalid constraint on pin 3");
    }

    #[test]
    fn retries_only_seed_dependent_nextpnr_failures() {
        assert!(retryable_nextpnr_failure("Info: routing\nERROR: Routing design failed.\n"));
        assert!(retryable_nextpnr_failure(
            "ERROR: Failed to route arc 1.0 of net 'clk', from A to B.\n"
        ));
        assert!(!retryable_nextpnr_failure(
            "ERROR: Unable to find legal placement for all cells, design is probably at utilisation limit.\n"
        ));
        assert!(!retryable_nextpnr_failure("ERROR: package pin does not exist\n"));
    }
}
