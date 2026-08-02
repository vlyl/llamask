pub mod detect;
pub mod docx_workflow;
pub mod image_workflow;
pub mod model;
pub mod pdf_workflow;
pub mod policy;
pub mod pptx_workflow;
pub mod sidecar;
pub mod text;
pub mod workflow;
pub mod xlsx_workflow;

pub use docx_workflow::{
    DocxWorkflowError, export_docx_task_with_runtimes, scan_docx_with_policy,
    scan_docx_with_policy_and_images, verify_docx_file_with_runtimes,
};
pub use image_workflow::{
    ImageWorkflowError, add_manual_image_mask, export_image_task_with_runtimes,
    remove_manual_image_group, render_image_task_preview, review_image_group,
    scan_image_with_policy, update_image_mask, verify_image_file_with_runtimes,
};
pub use model::{
    DocxEmbeddedImageTask, DocxTaskDraft, DocxVerificationReport, Finding, ImagePreview, ImageRect,
    ImageTaskDraft, ImageVerificationReport, PdfTaskDraft, PdfVerificationReport, PptxTaskDraft,
    PptxVerificationReport, TaskDraft, VerificationReport, XlsxTaskDraft, XlsxVerificationReport,
};
pub use pdf_workflow::{
    PdfScanProgress, PdfWorkflowError, export_pdf_task_with_runtimes, render_pdf_task_page_preview,
    scan_pdf_with_policy, scan_pdf_with_policy_and_progress, verify_pdf_file_with_runtimes,
};
pub use policy::{PolicyConfig, PolicyError};
pub use pptx_workflow::{
    PptxWorkflowError, export_pptx_task_with_runtimes, scan_pptx_with_policy,
    scan_pptx_with_policy_and_images, verify_pptx_file_with_runtimes,
};
pub use sidecar::{RuntimeRegistry, SidecarError};
pub use workflow::{
    WorkflowError, export_task, export_task_with_runtimes, render_task_with_runtimes,
    review_text_finding, scan_path, scan_path_with_policy, scan_text_with_policy, verify_file,
    verify_file_with_runtimes,
};
pub use xlsx_workflow::{
    XlsxWorkflowError, export_xlsx_task_with_runtimes, scan_xlsx_with_policy,
    verify_xlsx_file_with_runtimes,
};
