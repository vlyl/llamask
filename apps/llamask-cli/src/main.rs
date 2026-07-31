use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use clap::{Parser, Subcommand};
use llamask_core::{
    DocxTaskDraft, ImageTaskDraft, PolicyConfig, PptxTaskDraft, RuntimeRegistry, TaskDraft,
    XlsxTaskDraft, export_docx_task_with_runtimes, export_image_task_with_runtimes,
    export_pptx_task_with_runtimes, export_task_with_runtimes, export_xlsx_task_with_runtimes,
    render_task_with_runtimes, scan_docx_with_policy_and_images, scan_image_with_policy,
    scan_path_with_policy, scan_pptx_with_policy_and_images, scan_text_with_policy,
    scan_xlsx_with_policy, verify_docx_file_with_runtimes, verify_file_with_runtimes,
    verify_image_file_with_runtimes, verify_pptx_file_with_runtimes,
    verify_xlsx_file_with_runtimes,
};

const MAX_STDIN_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(name = "llamask", version, about = "完全离线的数据脱敏原型")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 扫描 UTF-8 TXT 或 Markdown，生成可编辑任务草稿
    Scan {
        input: PathBuf,
        #[arg(short, long)]
        task: PathBuf,
        /// 策略 JSON；省略时使用内置高召回策略
        #[arg(long)]
        policy: Option<PathBuf>,
        /// 本地模型运行注册表；省略时使用规则并记录模型降级提示
        #[arg(long)]
        runtimes: Option<PathBuf>,
    },
    /// 从标准输入读取纯文本，适合剪贴板和管道工作流
    ScanStdin {
        #[arg(short, long)]
        task: PathBuf,
        #[arg(long)]
        policy: Option<PathBuf>,
        #[arg(long)]
        runtimes: Option<PathBuf>,
    },
    /// 扫描 PNG/JPEG，生成带可编辑遮罩矩形的任务草稿
    ScanImage {
        input: PathBuf,
        #[arg(short, long)]
        task: PathBuf,
        #[arg(long)]
        policy: Option<PathBuf>,
        /// 包含 OCR 以及可选文本模型的本地运行注册表
        #[arg(long)]
        runtimes: PathBuf,
        /// 注册表中的 OCR 运行项 id
        #[arg(long, default_value = "pp_ocr_small")]
        ocr_runtime: String,
    },
    /// 扫描 DOCX 的正文、表格、页眉页脚、脚注、批注、图表和字段代码
    ScanDocx {
        input: PathBuf,
        #[arg(short, long)]
        task: PathBuf,
        #[arg(long)]
        policy: Option<PathBuf>,
        #[arg(long)]
        runtimes: Option<PathBuf>,
        /// 注册表中的 OCR 运行项 id；提供 runtimes 时递归扫描嵌入图片
        #[arg(long, default_value = "pp_ocr_small")]
        ocr_runtime: String,
    },
    /// 扫描 XLSX 单元格、公式、批注、隐藏内容、绘图文字和嵌入图片
    ScanXlsx {
        input: PathBuf,
        #[arg(short, long)]
        task: PathBuf,
        #[arg(long)]
        policy: Option<PathBuf>,
        #[arg(long)]
        runtimes: Option<PathBuf>,
        /// 注册表中的 OCR 运行项 id；提供 runtimes 时递归扫描嵌入图片
        #[arg(long, default_value = "pp_ocr_small")]
        ocr_runtime: String,
    },
    /// 扫描 PPTX 的页面、表格、备注、批注、隐藏页、母版、版式和嵌入图片
    ScanPptx {
        input: PathBuf,
        #[arg(short, long)]
        task: PathBuf,
        #[arg(long)]
        policy: Option<PathBuf>,
        #[arg(long)]
        runtimes: Option<PathBuf>,
        /// 注册表中的 OCR 运行项 id；提供 runtimes 时递归扫描嵌入图片
        #[arg(long, default_value = "pp_ocr_small")]
        ocr_runtime: String,
    },
    /// 按任务草稿生成脱敏副本；不会覆盖原件或已有文件
    Export {
        task: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// 使用同一组本地模型执行导出后独立复扫
        #[arg(long)]
        runtimes: Option<PathBuf>,
    },
    /// 生成 PNG/JPEG 实心打码副本，并用 OCR 独立复扫
    ExportImage {
        task: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        runtimes: PathBuf,
    },
    /// 生成最小 OOXML 改动的 DOCX 脱敏副本，并执行解包残留复扫
    ExportDocx {
        task: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        runtimes: Option<PathBuf>,
    },
    /// 生成最小 OOXML 改动的 XLSX 脱敏副本，并执行包内残留复扫
    ExportXlsx {
        task: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        runtimes: Option<PathBuf>,
    },
    /// 生成最小 OOXML 改动的 PPTX 脱敏副本，并执行包内残留复扫
    ExportPptx {
        task: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        runtimes: Option<PathBuf>,
    },
    /// 对已有副本重新运行规则和可选本地模型
    Verify {
        task: PathBuf,
        file: PathBuf,
        #[arg(long)]
        runtimes: Option<PathBuf>,
    },
    /// 对图片副本重新运行 OCR、规则和可选文本模型
    VerifyImage {
        task: PathBuf,
        file: PathBuf,
        #[arg(long)]
        runtimes: PathBuf,
    },
    /// 对 DOCX 副本执行解包文本、隐藏部件、元数据和 ZIP 结构复核
    VerifyDocx {
        task: PathBuf,
        file: PathBuf,
        #[arg(long)]
        runtimes: Option<PathBuf>,
    },
    /// 对 XLSX 副本执行单元格、公式、批注、图片、元数据和 ZIP 结构复核
    VerifyXlsx {
        task: PathBuf,
        file: PathBuf,
        #[arg(long)]
        runtimes: Option<PathBuf>,
    },
    /// 对 PPTX 副本执行页面、备注、批注、母版、图片、元数据和 ZIP 结构复核
    VerifyPptx {
        task: PathBuf,
        file: PathBuf,
        #[arg(long)]
        runtimes: Option<PathBuf>,
    },
    /// 把任务中的脱敏文本写到标准输出，适合复制回剪贴板
    Render {
        task: PathBuf,
        #[arg(long)]
        runtimes: Option<PathBuf>,
    },
    /// 创建或检查脱敏策略
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// 检查本地模型运行注册表
    Runtimes {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
}

#[derive(Debug, Subcommand)]
enum PolicyCommand {
    /// 生成一份可编辑的默认策略
    Init { output: PathBuf },
    /// 检查策略结构和取值
    Validate { file: PathBuf },
}

#[derive(Debug, Subcommand)]
enum RuntimeCommand {
    /// 检查本地模型运行注册表结构
    Validate { file: PathBuf },
    /// 检查模型程序、工作目录和资源 SHA-256
    Verify { file: PathBuf },
}

fn read_task(path: &Path) -> Result<TaskDraft, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn read_image_task(path: &Path) -> Result<ImageTaskDraft, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn read_docx_task(path: &Path) -> Result<DocxTaskDraft, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn read_xlsx_task(path: &Path) -> Result<XlsxTaskDraft, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn read_pptx_task(path: &Path) -> Result<PptxTaskDraft, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn read_policy(path: &Path) -> Result<PolicyConfig, Box<dyn std::error::Error>> {
    let policy: PolicyConfig = serde_json::from_slice(&std::fs::read(path)?)?;
    policy.validate()?;
    Ok(policy)
}

fn read_runtimes(
    path: Option<&Path>,
) -> Result<Option<RuntimeRegistry>, Box<dyn std::error::Error>> {
    path.map(RuntimeRegistry::from_path)
        .transpose()
        .map_err(Into::into)
}

fn read_stdin_text() -> Result<String, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take((MAX_STDIN_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_STDIN_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "标准输入超过 2 MiB 限制",
        )
        .into());
    }
    String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "标准输入不是有效 UTF-8").into()
    })
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Scan {
            input,
            task,
            policy,
            runtimes,
        } => {
            let policy = match policy {
                Some(path) => read_policy(&path)?,
                None => PolicyConfig::default(),
            };
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let draft = scan_path_with_policy(&input, &policy, runtimes.as_ref())?;
            let bytes = serde_json::to_vec_pretty(&draft)?;
            write_new(&task, &bytes)?;
            println!(
                "扫描完成：{} 个命中，{} 条运行提示；任务草稿包含敏感原文，请妥善保管。\n{}",
                draft.findings.len(),
                draft.diagnostics.len(),
                task.display()
            );
            for diagnostic in &draft.diagnostics {
                eprintln!(
                    "提示 [{}] {}",
                    diagnostic.detector_id.as_deref().unwrap_or("core"),
                    diagnostic.message
                );
            }
        }
        Command::ScanStdin {
            task,
            policy,
            runtimes,
        } => {
            let policy = match policy {
                Some(path) => read_policy(&path)?,
                None => PolicyConfig::default(),
            };
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let draft = scan_text_with_policy(read_stdin_text()?, &policy, runtimes.as_ref())?;
            let bytes = serde_json::to_vec_pretty(&draft)?;
            write_new(&task, &bytes)?;
            println!(
                "文本扫描完成：{} 个命中，{} 个待复核；任务草稿包含敏感原文，请妥善保管。\n{}",
                draft.findings.len(),
                draft
                    .findings
                    .iter()
                    .filter(|finding| !finding.reviewed)
                    .count(),
                task.display()
            );
        }
        Command::ScanImage {
            input,
            task,
            policy,
            runtimes,
            ocr_runtime,
        } => {
            let policy = match policy {
                Some(path) => read_policy(&path)?,
                None => PolicyConfig::default(),
            };
            let runtimes = RuntimeRegistry::from_path(&runtimes)?;
            let draft = scan_image_with_policy(&input, &policy, &runtimes, &ocr_runtime)?;
            let bytes = serde_json::to_vec_pretty(&draft)?;
            write_new(&task, &bytes)?;
            let logical_findings = draft
                .findings
                .iter()
                .map(|finding| finding.group_id.as_str())
                .collect::<BTreeSet<_>>()
                .len();
            let unreviewed = draft
                .findings
                .iter()
                .filter(|finding| !finding.reviewed)
                .map(|finding| finding.group_id.as_str())
                .collect::<BTreeSet<_>>()
                .len();
            println!(
                "图片扫描完成：{} 个逻辑命中、{} 个遮罩矩形，{} 个待复核；任务草稿包含 OCR 敏感原文，请妥善保管。\n{}",
                logical_findings,
                draft.findings.len(),
                unreviewed,
                task.display()
            );
        }
        Command::ScanDocx {
            input,
            task,
            policy,
            runtimes,
            ocr_runtime,
        } => {
            let policy = match policy {
                Some(path) => read_policy(&path)?,
                None => PolicyConfig::default(),
            };
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let draft = scan_docx_with_policy_and_images(
                &input,
                &policy,
                runtimes.as_ref(),
                runtimes.as_ref().map(|_| ocr_runtime.as_str()),
            )?;
            let bytes = serde_json::to_vec_pretty(&draft)?;
            write_new(&task, &bytes)?;
            println!(
                "DOCX 扫描完成：{} 个文本命中、{} 个图片逻辑命中，{} 个待复核，{} 个文本部分、{} 张嵌入图片；任务草稿包含敏感原文，请妥善保管。\n{}",
                draft.findings.len(),
                draft
                    .embedded_images
                    .iter()
                    .flat_map(|image| image
                        .task
                        .findings
                        .iter()
                        .map(move |finding| (&image.entry_name, &finding.group_id)))
                    .collect::<BTreeSet<_>>()
                    .len(),
                draft
                    .findings
                    .iter()
                    .filter(|finding| !finding.reviewed)
                    .count()
                    + draft
                        .embedded_images
                        .iter()
                        .flat_map(|image| image
                            .task
                            .findings
                            .iter()
                            .filter(|finding| !finding.reviewed)
                            .map(move |finding| (&image.entry_name, &finding.group_id)))
                        .collect::<BTreeSet<_>>()
                        .len(),
                draft.document.parts.len(),
                draft.embedded_images.len(),
                task.display()
            );
            for diagnostic in &draft.diagnostics {
                eprintln!(
                    "提示 [{}] {}",
                    diagnostic.detector_id.as_deref().unwrap_or("docx-core"),
                    diagnostic.message
                );
            }
        }
        Command::ScanXlsx {
            input,
            task,
            policy,
            runtimes,
            ocr_runtime,
        } => {
            let policy = match policy {
                Some(path) => read_policy(&path)?,
                None => PolicyConfig::default(),
            };
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let draft = scan_xlsx_with_policy(
                &input,
                &policy,
                runtimes.as_ref(),
                runtimes.as_ref().map(|_| ocr_runtime.as_str()),
            )?;
            let bytes = serde_json::to_vec_pretty(&draft)?;
            write_new(&task, &bytes)?;
            let image_findings = draft
                .embedded_images
                .iter()
                .flat_map(|image| {
                    image
                        .task
                        .findings
                        .iter()
                        .map(move |finding| (&image.entry_name, &finding.group_id))
                })
                .collect::<BTreeSet<_>>()
                .len();
            let unreviewed = draft
                .findings
                .iter()
                .filter(|finding| !finding.reviewed)
                .count()
                + draft
                    .embedded_images
                    .iter()
                    .flat_map(|image| {
                        image
                            .task
                            .findings
                            .iter()
                            .filter(|finding| !finding.reviewed)
                            .map(move |finding| (&image.entry_name, &finding.group_id))
                    })
                    .collect::<BTreeSet<_>>()
                    .len();
            println!(
                "XLSX 扫描完成：{} 个结构化命中、{} 个图片逻辑命中，{} 个待复核，{} 个内容部分、{} 张嵌入图片；任务草稿包含敏感原文，请妥善保管。\n{}",
                draft.findings.len(),
                image_findings,
                unreviewed,
                draft.document.parts.len(),
                draft.embedded_images.len(),
                task.display()
            );
            for diagnostic in &draft.diagnostics {
                eprintln!(
                    "提示 [{}] {}",
                    diagnostic.detector_id.as_deref().unwrap_or("xlsx-core"),
                    diagnostic.message
                );
            }
        }
        Command::ScanPptx {
            input,
            task,
            policy,
            runtimes,
            ocr_runtime,
        } => {
            let policy = match policy {
                Some(path) => read_policy(&path)?,
                None => PolicyConfig::default(),
            };
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let draft = scan_pptx_with_policy_and_images(
                &input,
                &policy,
                runtimes.as_ref(),
                runtimes.as_ref().map(|_| ocr_runtime.as_str()),
            )?;
            let bytes = serde_json::to_vec_pretty(&draft)?;
            write_new(&task, &bytes)?;
            let image_findings = draft
                .embedded_images
                .iter()
                .flat_map(|image| {
                    image
                        .task
                        .findings
                        .iter()
                        .map(move |finding| (&image.entry_name, &finding.group_id))
                })
                .collect::<BTreeSet<_>>()
                .len();
            let unreviewed = draft
                .findings
                .iter()
                .filter(|finding| !finding.reviewed)
                .count()
                + draft
                    .embedded_images
                    .iter()
                    .flat_map(|image| {
                        image
                            .task
                            .findings
                            .iter()
                            .filter(|finding| !finding.reviewed)
                            .map(move |finding| (&image.entry_name, &finding.group_id))
                    })
                    .collect::<BTreeSet<_>>()
                    .len();
            println!(
                "PPTX 扫描完成：{} 个结构化命中、{} 个图片逻辑命中，{} 个待复核，{} 个内容部分、{} 页、{} 张嵌入图片；任务草稿包含敏感原文，请妥善保管。\n{}",
                draft.findings.len(),
                image_findings,
                unreviewed,
                draft.document.parts.len(),
                draft.document.source.slides,
                draft.embedded_images.len(),
                task.display()
            );
            for diagnostic in &draft.diagnostics {
                eprintln!(
                    "提示 [{}] {}",
                    diagnostic.detector_id.as_deref().unwrap_or("pptx-core"),
                    diagnostic.message
                );
            }
        }
        Command::Export {
            task,
            output,
            runtimes,
        } => {
            let draft = read_task(&task)?;
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let report = export_task_with_runtimes(&draft, &output, runtimes.as_ref())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::ExportImage {
            task,
            output,
            runtimes,
        } => {
            let draft = read_image_task(&task)?;
            let runtimes = RuntimeRegistry::from_path(&runtimes)?;
            let report = export_image_task_with_runtimes(&draft, &output, &runtimes)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::ExportDocx {
            task,
            output,
            runtimes,
        } => {
            let draft = read_docx_task(&task)?;
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let report = export_docx_task_with_runtimes(&draft, &output, runtimes.as_ref())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::ExportXlsx {
            task,
            output,
            runtimes,
        } => {
            let draft = read_xlsx_task(&task)?;
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let report = export_xlsx_task_with_runtimes(&draft, &output, runtimes.as_ref())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::ExportPptx {
            task,
            output,
            runtimes,
        } => {
            let draft = read_pptx_task(&task)?;
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let report = export_pptx_task_with_runtimes(&draft, &output, runtimes.as_ref())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Verify {
            task,
            file,
            runtimes,
        } => {
            let draft = read_task(&task)?;
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let report = verify_file_with_runtimes(&draft, &file, runtimes.as_ref())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.passed {
                std::process::exit(2);
            }
        }
        Command::VerifyImage {
            task,
            file,
            runtimes,
        } => {
            let draft = read_image_task(&task)?;
            let runtimes = RuntimeRegistry::from_path(&runtimes)?;
            let report = verify_image_file_with_runtimes(&draft, &file, &runtimes)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.passed {
                std::process::exit(2);
            }
        }
        Command::VerifyDocx {
            task,
            file,
            runtimes,
        } => {
            let draft = read_docx_task(&task)?;
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let report = verify_docx_file_with_runtimes(&draft, &file, runtimes.as_ref())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.passed {
                std::process::exit(2);
            }
        }
        Command::VerifyXlsx {
            task,
            file,
            runtimes,
        } => {
            let draft = read_xlsx_task(&task)?;
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let report = verify_xlsx_file_with_runtimes(&draft, &file, runtimes.as_ref())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.passed {
                std::process::exit(2);
            }
        }
        Command::VerifyPptx {
            task,
            file,
            runtimes,
        } => {
            let draft = read_pptx_task(&task)?;
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let report = verify_pptx_file_with_runtimes(&draft, &file, runtimes.as_ref())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.passed {
                std::process::exit(2);
            }
        }
        Command::Render { task, runtimes } => {
            let draft = read_task(&task)?;
            let runtimes = read_runtimes(runtimes.as_deref())?;
            let (text, report) = render_task_with_runtimes(&draft, runtimes.as_ref())?;
            std::io::stdout().lock().write_all(text.as_bytes())?;
            if !report.complete {
                eprintln!("提示：本次复扫未运行全部可选模型检测器。");
            }
        }
        Command::Policy { command } => match command {
            PolicyCommand::Init { output } => {
                let mut bytes = serde_json::to_vec_pretty(&PolicyConfig::default())?;
                bytes.push(b'\n');
                write_new(&output, &bytes)?;
                println!("已生成默认策略：{}", output.display());
            }
            PolicyCommand::Validate { file } => {
                let policy = read_policy(&file)?;
                println!(
                    "策略有效：{}（{} 类实体，{} 个模型检测器）",
                    policy.id,
                    policy.entities.len(),
                    policy.detectors.len()
                );
            }
        },
        Command::Runtimes { command } => match command {
            RuntimeCommand::Validate { file } => {
                let registry = RuntimeRegistry::from_path(&file)?;
                println!("模型运行注册表有效：{} 个检测器", registry.detectors.len());
            }
            RuntimeCommand::Verify { file } => {
                let registry = RuntimeRegistry::from_path(&file)?;
                registry.verify_installation()?;
                println!("模型运行环境完整：{} 个检测器", registry.detectors.len());
            }
        },
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("操作失败：{error}");
        std::process::exit(1);
    }
}
