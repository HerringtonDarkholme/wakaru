use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use wakaru_core::module::{module_mapping, module_meta_map, Module, ModuleMapping, ModuleMetaMap};
use wakaru_core::source::SourceFile;
use wakaru_unminify::pipeline::{run_default_transformations, PipelineParams};
use wakaru_unpacker::unpack::{unpack_source, UnpackResult};

use crate::args::{Cli, Command};
use crate::output::{write_file, write_module};
use crate::path::{common_base_dir, ensure_output_available, relative_path, resolve_file_globs};
use crate::perf::format_elapsed_ms;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Feature {
    Unpacker,
    Unminify,
}

pub fn run(cli: Cli) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let (features, inputs) = feature_plan(&cli);

    if inputs.is_empty() {
        bail!("no input files specified");
    }

    let input_paths = resolve_file_globs(inputs)?;
    let single_feature = features.len() == 1;
    let output_base = PathBuf::from(&cli.output);
    let unpacker_output = cli
        .unpacker_output
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if single_feature {
                output_base.clone()
            } else {
                output_base.join("unpack")
            }
        });
    let unminify_output = cli
        .unminify_output
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if single_feature {
                output_base.clone()
            } else {
                output_base.join("unminify")
            }
        });

    if features.contains(&Feature::Unpacker) {
        ensure_output_available(&unpacker_output, cli.force)?;
    }
    if features.contains(&Feature::Unminify) {
        ensure_output_available(&unminify_output, cli.force)?;
    }

    println!("Selected features: {}", feature_names(&features).join(", "));

    let mut unminify_inputs = Vec::new();
    let mut modules = Vec::new();

    if features.contains(&Feature::Unpacker) {
        let start = Instant::now();
        let mut total_modules = 0usize;

        for input_path in &input_paths {
            println!("Unpacking {}", relative_path(&cwd, input_path));
            let source = SourceFile::read(input_path)?;
            let result = unpack_source(&source)?;
            total_modules += result.modules.len();
            let written = write_unpack_result(&unpacker_output, result)?;
            unminify_inputs.extend(written.files);
            modules.extend(written.modules);
        }

        println!(
            "Successfully generated {} modules ({})",
            total_modules,
            format_elapsed_ms(start.elapsed().as_secs_f64() * 1000.0)
        );
        println!(
            "Output directory: {}",
            relative_path(&cwd, &unpacker_output)
        );
    }

    if features.contains(&Feature::Unminify) {
        if !features.contains(&Feature::Unpacker) {
            unminify_inputs = input_paths;
        }

        if unminify_inputs.is_empty() {
            bail!("no files available for unminify");
        }

        let common_base = common_base_dir(&unminify_inputs)
            .context("could not find common base directory with input paths")?;
        let params = PipelineParams {
            module_mapping: module_mapping(&modules),
            module_meta: module_meta_map(&modules),
        };
        let start = Instant::now();

        for input_path in &unminify_inputs {
            println!("Unminifying {}", relative_path(&cwd, input_path));
            let source = SourceFile::read(input_path)?;
            let result = run_default_transformations(&source, params.clone())?;
            let relative = input_path.strip_prefix(&common_base).unwrap_or(input_path);
            let output_path = unminify_output.join(relative);
            write_file(&output_path, &result.code)?;
        }

        println!(
            "Successfully unminified {} files ({})",
            unminify_inputs.len(),
            format_elapsed_ms(start.elapsed().as_secs_f64() * 1000.0)
        );
        println!(
            "Output directory: {}",
            relative_path(&cwd, &unminify_output)
        );
    }

    Ok(())
}

struct WrittenUnpackResult {
    files: Vec<PathBuf>,
    modules: Vec<Module>,
    _module_id_mapping: ModuleMapping,
    _module_meta: ModuleMetaMap,
}

fn write_unpack_result(output_dir: &Path, result: UnpackResult) -> Result<WrittenUnpackResult> {
    let mut files = Vec::new();
    for module in &result.modules {
        files.push(write_module(output_dir, module)?);
    }

    let module_meta = module_meta_map(&result.modules);

    Ok(WrittenUnpackResult {
        files,
        modules: result.modules,
        _module_id_mapping: result.module_id_mapping,
        _module_meta: module_meta,
    })
}

fn feature_plan(cli: &Cli) -> (Vec<Feature>, &[String]) {
    match &cli.command {
        Some(Command::All { inputs }) => (vec![Feature::Unpacker, Feature::Unminify], inputs),
        Some(Command::Unpacker { inputs }) => (vec![Feature::Unpacker], inputs),
        Some(Command::Unminify { inputs }) => (vec![Feature::Unminify], inputs),
        None => (vec![Feature::Unpacker, Feature::Unminify], &cli.inputs),
    }
}

fn feature_names(features: &[Feature]) -> Vec<&'static str> {
    features
        .iter()
        .map(|feature| match feature {
            Feature::Unpacker => "Unpacker",
            Feature::Unminify => "Unminify",
        })
        .collect()
}
