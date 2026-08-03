import { useEffect, useMemo, useState } from "react";

import { ChevronRightIcon, CloseIcon, ShieldIcon } from "./icons";
import type { DesktopTextReview, ImportedFile } from "./types";

const entityLabels: Record<string, string> = {
  PERSON_NAME: "姓名",
  ORG_NAME: "机构名称",
  ADDRESS: "地址",
  PHONE_NUMBER: "电话号码",
  EMAIL: "电子邮箱",
  CN_ID_NUMBER: "身份证件号",
  BANK_CARD_NUMBER: "银行卡号",
  FINANCIAL_AMOUNT: "财务金额",
  SALARY_AMOUNT: "薪酬金额",
  BUSINESS_METRIC: "业务指标",
  PROJECT_CODE: "项目代号",
  CONTRACT_ID: "合同编号",
  CUSTOMER_NAME: "客户名称",
};

interface TextReviewWorkspaceProps {
  file: ImportedFile;
  review: DesktopTextReview;
  busy: boolean;
  exporting: boolean;
  onClose: () => void;
  onSetFinding: (
    findingId: string,
    selected: boolean,
    replacement?: string,
  ) => Promise<void>;
  onOpenEmbeddedImages?: () => Promise<void>;
  onExport: () => Promise<void>;
}

export function TextReviewWorkspace({
  file,
  review,
  busy,
  exporting,
  onClose,
  onSetFinding,
  onOpenEmbeddedImages,
  onExport,
}: TextReviewWorkspaceProps) {
  const initialReplacements = useMemo(
    () =>
      Object.fromEntries(
        review.findings.map((finding) => [finding.findingId, finding.replacement]),
      ),
    [review.findings],
  );
  const [replacements, setReplacements] = useState<Record<string, string>>(initialReplacements);

  useEffect(() => {
    if (!busy) setReplacements(initialReplacements);
  }, [busy, initialReplacements]);

  const pendingFindings = review.findings.filter((finding) => !finding.reviewed).length;
  const pendingResults = pendingFindings + review.unreviewedImageGroups;
  const clipboardTask = file.sourceKind === "clipboard";
  const wordTask = file.kind === "word";

  return (
    <div className="review-backdrop" role="dialog" aria-modal="true" aria-label="文本脱敏结果复核">
      <section className="review-workspace text-review-workspace">
        <header className="review-header">
          <div>
            <p className="eyebrow">本地文本复核 · 有界上下文</p>
            <h2>{file.displayName}</h2>
            <span>
              {review.totalCharacters.toLocaleString()} 个字符 · {review.findings.length} 个文字结果
              {review.embeddedImageCount > 0 ? ` · ${review.embeddedImageCount} 张内嵌图片` : ""} · {pendingResults} 个待确认
            </span>
          </div>
          <button className="icon-button" type="button" onClick={onClose} aria-label="关闭复核">
            <CloseIcon />
          </button>
        </header>

        <div className="text-review-body">
          <div className="review-security-note text-review-security-note">
            <ShieldIcon />
            <span>
              每个结果只显示命中内容及前后各 80 个字符；源文件路径、OOXML 定位和完整文档不会进入界面状态或浏览器存储。
            </span>
          </div>

          {review.embeddedImageCount > 0 && onOpenEmbeddedImages && (
            <div className="embedded-image-review-card">
              <div>
                <ShieldIcon />
                <span>
                  <strong>{review.embeddedImageCount} 张内嵌图片</strong>
                  <small>
                    {review.unreviewedImageGroups > 0
                      ? `${review.unreviewedImageGroups} 个遮罩结果仍需确认`
                      : "可以逐张检查并补充手动遮罩"}
                  </small>
                </span>
              </div>
              <button
                className="secondary-button"
                type="button"
                disabled={busy}
                onClick={() => void onOpenEmbeddedImages()}
              >
                复核内嵌图片
              </button>
            </div>
          )}

          <div className="text-finding-list">
            {review.findings.length === 0 && (
              <div className="empty-text-review">
                <ShieldIcon />
                <strong>
                  {wordTask ? "没有发现需要处理的文字敏感信息" : "没有发现需要处理的敏感信息"}
                </strong>
                <span>
                  {review.embeddedImageCount > 0
                    ? "内嵌图片仍可逐张检查；导出前会执行独立残留复扫。"
                    : "仍会在导出前执行一次独立残留复扫。"}
                </span>
              </div>
            )}
            {review.findings.map((finding, index) => {
              const replacement = replacements[finding.findingId] ?? "";
              return (
                <article
                  className={`text-finding-card ${finding.reviewed ? "reviewed" : "pending"}`}
                  key={finding.findingId}
                >
                  <div className="text-finding-heading">
                    <div>
                      <span className="finding-number">{String(index + 1).padStart(2, "0")}</span>
                      <strong>{entityLabels[finding.entityType] ?? finding.entityType}</strong>
                      {finding.sectionLabel && (
                        <span className="finding-section">{finding.sectionLabel}</span>
                      )}
                      <small>{Math.round(finding.confidence * 100)}% 置信度</small>
                    </div>
                    <em>
                      {!finding.reviewed
                        ? "待确认"
                        : finding.selected
                          ? "应用替换"
                          : "保留原文"}
                    </em>
                  </div>

                  <p className="text-context" aria-label="命中上下文">
                    <span>{finding.contextBefore}</span>
                    <mark>{finding.matchedText}</mark>
                    <span>{finding.contextAfter}</span>
                  </p>

                  <label className="replacement-field">
                    <span>替换为</span>
                    <input
                      type="text"
                      maxLength={256}
                      value={replacement}
                      disabled={busy}
                      onChange={(event) =>
                        setReplacements((current) => ({
                          ...current,
                          [finding.findingId]: event.target.value,
                        }))
                      }
                    />
                    <small>{replacement.length}/256</small>
                  </label>

                  <div className="finding-actions text-finding-actions">
                    <button
                      className="secondary-button"
                      type="button"
                      disabled={busy}
                      onClick={() =>
                        void onSetFinding(finding.findingId, true, replacement)
                      }
                    >
                      应用替换
                    </button>
                    <button
                      className="text-button"
                      type="button"
                      disabled={busy}
                      onClick={() => void onSetFinding(finding.findingId, false)}
                    >
                      保留原文
                    </button>
                  </div>
                </article>
              );
            })}
          </div>
        </div>

        <footer className="review-footer">
          <span>
            {pendingResults > 0
              ? `还有 ${pendingResults} 个结果需要确认`
              : clipboardTask
                ? "所有结果已复核，可以安全复制"
                : "所有结果已复核，可以执行安全导出"}
          </span>
          <button
            className="primary-button"
            type="button"
            disabled={busy || exporting || pendingResults > 0}
            onClick={() => void onExport()}
          >
            {exporting
              ? clipboardTask
                ? "正在复制并复检…"
                : "正在导出并复检…"
              : clipboardTask
                ? "复制脱敏结果"
                : "保存安全副本"}
            <ChevronRightIcon />
          </button>
        </footer>
      </section>
    </div>
  );
}
