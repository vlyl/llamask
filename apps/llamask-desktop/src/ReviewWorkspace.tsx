import { useEffect, useMemo, useRef, useState } from "react";

import { ChevronRightIcon, CloseIcon, PlusIcon, ShieldIcon, TrashIcon } from "./icons";
import type {
  DesktopReviewFinding,
  DesktopReviewPage,
  ImageRect,
  ImportedFile,
} from "./types";

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
  MANUAL_MASK: "手动遮罩",
};

interface ReviewWorkspaceProps {
  file: ImportedFile;
  page: DesktopReviewPage;
  busy: boolean;
  exporting: boolean;
  canExport: boolean;
  onClose: () => void;
  onPageChange: (pageNumber: number) => Promise<void>;
  onSetGroup: (groupId: string, selected: boolean) => Promise<void>;
  onUpdateMask: (findingId: string, maskRect: ImageRect) => Promise<void>;
  onAddMask: (maskRect: ImageRect) => Promise<void>;
  onRemoveMask: (groupId: string) => Promise<void>;
  onExport: () => Promise<void>;
}

interface PointerEdit {
  pointerId: number;
  findingId: string;
  mode: "move" | "resize";
  startClientX: number;
  startClientY: number;
  original: ImageRect;
  latest: ImageRect;
}

interface DrawEdit {
  pointerId: number;
  startX: number;
  startY: number;
}

function clamp(value: number, minimum: number, maximum: number) {
  return Math.max(minimum, Math.min(maximum, Math.round(value)));
}

function validRect(rect: ImageRect, width: number, height: number) {
  return (
    Object.values(rect).every(Number.isFinite) &&
    rect.x0 >= 0 &&
    rect.y0 >= 0 &&
    rect.x1 <= width &&
    rect.y1 <= height &&
    rect.x1 - rect.x0 >= 2 &&
    rect.y1 - rect.y0 >= 2
  );
}

function groupState(findings: DesktopReviewFinding[]) {
  const groups = new Map<
    string,
    {
      id: string;
      entityType: string;
      findings: DesktopReviewFinding[];
      selected: boolean;
      reviewed: boolean;
      manual: boolean;
      confidence: number;
    }
  >();
  for (const finding of findings) {
    const group = groups.get(finding.groupId);
    if (group) {
      group.findings.push(finding);
      group.selected &&= finding.selected;
      group.reviewed &&= finding.reviewed;
      group.manual &&= finding.manual;
      group.confidence = Math.min(group.confidence, finding.confidence);
    } else {
      groups.set(finding.groupId, {
        id: finding.groupId,
        entityType: finding.entityType,
        findings: [finding],
        selected: finding.selected,
        reviewed: finding.reviewed,
        manual: finding.manual,
        confidence: finding.confidence,
      });
    }
  }
  return [...groups.values()];
}

export function ReviewWorkspace({
  file,
  page,
  busy,
  exporting,
  canExport,
  onClose,
  onPageChange,
  onSetGroup,
  onUpdateMask,
  onAddMask,
  onRemoveMask,
  onExport,
}: ReviewWorkspaceProps) {
  const overlayRef = useRef<HTMLDivElement>(null);
  const [findings, setFindings] = useState(page.findings);
  const [activeFindingId, setActiveFindingId] = useState<string | null>(
    page.findings[0]?.findingId ?? null,
  );
  const [addMode, setAddMode] = useState(false);
  const [pointerEdit, setPointerEdit] = useState<PointerEdit | null>(null);
  const [drawEdit, setDrawEdit] = useState<DrawEdit | null>(null);
  const [drawRect, setDrawRect] = useState<ImageRect | null>(null);

  useEffect(() => {
    setFindings(page.findings);
    setActiveFindingId((current) =>
      page.findings.some((finding) => finding.findingId === current)
        ? current
        : page.findings[0]?.findingId ?? null,
    );
    setAddMode(false);
    setPointerEdit(null);
    setDrawEdit(null);
    setDrawRect(null);
  }, [page]);

  useEffect(() => {
    if (!busy) setFindings(page.findings);
  }, [busy, page.findings]);

  const groups = useMemo(() => groupState(findings), [findings]);
  const activeFinding = findings.find((finding) => finding.findingId === activeFindingId);
  const pendingGroups = groups.filter((group) => !group.reviewed).length;
  const embeddedImage =
    file.kind === "word" || file.kind === "spreadsheet" || file.kind === "presentation";

  const sourcePoint = (clientX: number, clientY: number) => {
    const bounds = overlayRef.current?.getBoundingClientRect();
    if (!bounds || bounds.width === 0 || bounds.height === 0) return null;
    return {
      x: clamp(((clientX - bounds.left) / bounds.width) * page.width, 0, page.width),
      y: clamp(((clientY - bounds.top) / bounds.height) * page.height, 0, page.height),
      scaleX: page.width / bounds.width,
      scaleY: page.height / bounds.height,
    };
  };

  const updateLocalRect = (findingId: string, maskRect: ImageRect) => {
    setFindings((current) =>
      current.map((finding) =>
        finding.findingId === findingId ? { ...finding, maskRect } : finding,
      ),
    );
  };

  const rectAtPointer = (
    edit: PointerEdit,
    clientX: number,
    clientY: number,
  ): ImageRect | null => {
    const point = sourcePoint(clientX, clientY);
    if (!point) return null;
    const dx = (clientX - edit.startClientX) * point.scaleX;
    const dy = (clientY - edit.startClientY) * point.scaleY;
    const original = edit.original;
    if (edit.mode === "resize") {
      return {
        ...original,
        x1: clamp(original.x1 + dx, original.x0 + 2, page.width),
        y1: clamp(original.y1 + dy, original.y0 + 2, page.height),
      };
    }
    const width = original.x1 - original.x0;
    const height = original.y1 - original.y0;
    const x0 = clamp(original.x0 + dx, 0, page.width - width);
    const y0 = clamp(original.y0 + dy, 0, page.height - height);
    return { x0, y0, x1: x0 + width, y1: y0 + height };
  };

  const beginFindingEdit = (
    event: React.PointerEvent<HTMLDivElement>,
    finding: DesktopReviewFinding,
    mode: "move" | "resize",
  ) => {
    if (busy || addMode || !finding.selected) return;
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    setActiveFindingId(finding.findingId);
    setPointerEdit({
      pointerId: event.pointerId,
      findingId: finding.findingId,
      mode,
      startClientX: event.clientX,
      startClientY: event.clientY,
      original: finding.maskRect,
      latest: finding.maskRect,
    });
  };

  const moveFinding = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!pointerEdit || pointerEdit.pointerId !== event.pointerId) return;
    const maskRect = rectAtPointer(pointerEdit, event.clientX, event.clientY);
    if (!maskRect) return;
    updateLocalRect(pointerEdit.findingId, maskRect);
    setPointerEdit((current) =>
      current && current.pointerId === event.pointerId ? { ...current, latest: maskRect } : current,
    );
  };

  const finishFindingEdit = async (event: React.PointerEvent<HTMLDivElement>) => {
    if (!pointerEdit || pointerEdit.pointerId !== event.pointerId) return;
    const findingId = pointerEdit.findingId;
    const maskRect =
      rectAtPointer(pointerEdit, event.clientX, event.clientY) ?? pointerEdit.latest;
    setPointerEdit(null);
    if (!validRect(maskRect, page.width, page.height)) return;
    try {
      await onUpdateMask(findingId, maskRect);
    } catch {
      setFindings(page.findings);
    }
  };

  const beginDraw = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!addMode || busy || event.target !== event.currentTarget) return;
    const point = sourcePoint(event.clientX, event.clientY);
    if (!point) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    setDrawEdit({ pointerId: event.pointerId, startX: point.x, startY: point.y });
    setDrawRect({ x0: point.x, y0: point.y, x1: point.x + 2, y1: point.y + 2 });
  };

  const moveDraw = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!drawEdit || drawEdit.pointerId !== event.pointerId) return;
    const point = sourcePoint(event.clientX, event.clientY);
    if (!point) return;
    setDrawRect({
      x0: Math.min(drawEdit.startX, point.x),
      y0: Math.min(drawEdit.startY, point.y),
      x1: Math.max(drawEdit.startX, point.x),
      y1: Math.max(drawEdit.startY, point.y),
    });
  };

  const finishDraw = async (event: React.PointerEvent<HTMLDivElement>) => {
    if (!drawEdit || drawEdit.pointerId !== event.pointerId) return;
    const point = sourcePoint(event.clientX, event.clientY);
    const completed = point
      ? {
          x0: Math.min(drawEdit.startX, point.x),
          y0: Math.min(drawEdit.startY, point.y),
          x1: Math.max(drawEdit.startX, point.x),
          y1: Math.max(drawEdit.startY, point.y),
        }
      : drawRect;
    setDrawEdit(null);
    setDrawRect(null);
    setAddMode(false);
    if (!completed || !validRect(completed, page.width, page.height)) return;
    await onAddMask(completed);
  };

  const editCoordinate = (key: keyof ImageRect, value: number) => {
    if (!activeFinding) return;
    const next = { ...activeFinding.maskRect, [key]: value };
    updateLocalRect(activeFinding.findingId, next);
  };

  const saveCoordinates = async () => {
    const current = findings.find((finding) => finding.findingId === activeFindingId);
    if (!current || !validRect(current.maskRect, page.width, page.height)) return;
    await onUpdateMask(current.findingId, current.maskRect);
  };

  return (
    <div className="review-backdrop" role="dialog" aria-modal="true" aria-label="脱敏结果复核">
      <section className="review-workspace">
        <header className="review-header">
          <div>
            <p className="eyebrow">本地复核 · 不写入浏览器存储</p>
            <h2>{file.displayName}</h2>
            <span>
              {embeddedImage
                ? `内嵌图片 ${page.pageNumber} / ${page.pageCount}`
                : `第 ${page.pageNumber} / ${page.pageCount} 页`} · {groups.length} 个结果 · {pendingGroups} 个待确认
            </span>
          </div>
          <div className="review-header-actions">
            <button
              className={`secondary-button ${addMode ? "active" : ""}`}
              type="button"
              onClick={() => setAddMode((current) => !current)}
              disabled={busy}
            >
              <PlusIcon /> {addMode ? "在页面上拖出矩形" : "新增遮罩"}
            </button>
            <button className="icon-button" type="button" onClick={onClose} aria-label="关闭复核">
              <CloseIcon />
            </button>
          </div>
        </header>

        <div className="review-body">
          <div className="preview-column">
            <div className={`page-preview ${addMode ? "drawing" : ""}`}>
              <img
                src={page.imageDataUrl}
                alt={embeddedImage ? "Office 内嵌图片复核" : "本地复核页面"}
                draggable={false}
              />
              <div
                className="mask-overlay"
                ref={overlayRef}
                onPointerDown={beginDraw}
                onPointerMove={moveDraw}
                onPointerUp={(event) => void finishDraw(event)}
              >
                {findings.map((finding) => (
                  <div
                    className={`mask-rect ${finding.selected ? "selected" : "kept"} ${finding.reviewed ? "reviewed" : "pending"} ${finding.findingId === activeFindingId ? "active" : ""}`}
                    key={finding.findingId}
                    style={{
                      left: `${(finding.maskRect.x0 / page.width) * 100}%`,
                      top: `${(finding.maskRect.y0 / page.height) * 100}%`,
                      width: `${((finding.maskRect.x1 - finding.maskRect.x0) / page.width) * 100}%`,
                      height: `${((finding.maskRect.y1 - finding.maskRect.y0) / page.height) * 100}%`,
                    }}
                    onPointerDown={(event) => beginFindingEdit(event, finding, "move")}
                    onPointerMove={moveFinding}
                    onPointerUp={(event) => void finishFindingEdit(event)}
                    onClick={(event) => {
                      event.stopPropagation();
                      setActiveFindingId(finding.findingId);
                    }}
                  >
                    {finding.selected && (
                      <div
                        className="mask-resize-handle"
                        onPointerDown={(event) => beginFindingEdit(event, finding, "resize")}
                        onPointerMove={moveFinding}
                        onPointerUp={(event) => void finishFindingEdit(event)}
                      />
                    )}
                  </div>
                ))}
                {drawRect && (
                  <div
                    className="mask-rect manual-draft"
                    style={{
                      left: `${(drawRect.x0 / page.width) * 100}%`,
                      top: `${(drawRect.y0 / page.height) * 100}%`,
                      width: `${((drawRect.x1 - drawRect.x0) / page.width) * 100}%`,
                      height: `${((drawRect.y1 - drawRect.y0) / page.height) * 100}%`,
                    }}
                  />
                )}
              </div>
            </div>

            <div className="page-controls">
              <button
                className="secondary-button"
                type="button"
                disabled={busy || page.pageNumber <= 1}
                onClick={() => void onPageChange(page.pageNumber - 1)}
              >
                {embeddedImage ? "上一张" : "上一页"}
              </button>
              <span>拖动遮罩可移动，右下角手柄可缩放</span>
              <button
                className="secondary-button"
                type="button"
                disabled={busy || page.pageNumber >= page.pageCount}
                onClick={() => void onPageChange(page.pageNumber + 1)}
              >
                {embeddedImage ? "下一张" : "下一页"}
              </button>
            </div>
          </div>

          <aside className="review-sidebar">
            <div className="review-security-note">
              <ShieldIcon />
              <span>这里只按需显示当前页图像和坐标，不传输 OCR 原文或源文件路径。</span>
            </div>

            <div className="finding-list">
              {groups.length === 0 && <p className="empty-findings">本页没有自动检测结果，可手动新增遮罩。</p>}
              {groups.map((group) => (
                <article
                  className={`finding-card ${group.reviewed ? "reviewed" : "pending"}`}
                  key={group.id}
                  onClick={() => setActiveFindingId(group.findings[0]?.findingId ?? null)}
                >
                  <div>
                    <strong>{entityLabels[group.entityType] ?? group.entityType}</strong>
                    <span>{Math.round(group.confidence * 100)}% 置信度 · {group.findings.length} 个矩形</span>
                  </div>
                  <em>
                    {!group.reviewed ? "待确认" : group.selected ? "应用遮罩" : "保留原文"}
                  </em>
                  <div className="finding-actions">
                    {group.manual ? (
                      <button
                        className="text-button danger"
                        type="button"
                        disabled={busy}
                        onClick={(event) => {
                          event.stopPropagation();
                          void onRemoveMask(group.id);
                        }}
                      >
                        <TrashIcon /> 删除手动遮罩
                      </button>
                    ) : (
                      <>
                        <button
                          className="secondary-button"
                          type="button"
                          disabled={busy}
                          onClick={(event) => {
                            event.stopPropagation();
                            void onSetGroup(group.id, true);
                          }}
                        >
                          应用遮罩
                        </button>
                        <button
                          className="text-button"
                          type="button"
                          disabled={busy}
                          onClick={(event) => {
                            event.stopPropagation();
                            void onSetGroup(group.id, false);
                          }}
                        >
                          保留原文
                        </button>
                      </>
                    )}
                  </div>
                </article>
              ))}
            </div>

            {activeFinding && activeFinding.selected && (
              <div className="coordinate-editor">
                <strong>精确调整选中遮罩</strong>
                <div>
                  {(["x0", "y0", "x1", "y1"] as const).map((key) => (
                    <label key={key}>
                      {key.toUpperCase()}
                      <input
                        type="number"
                        min={0}
                        max={key.startsWith("x") ? page.width : page.height}
                        value={activeFinding.maskRect[key]}
                        onChange={(event) => editCoordinate(key, Number(event.target.value))}
                      />
                    </label>
                  ))}
                </div>
                <button
                  className="secondary-button"
                  type="button"
                  disabled={busy || !validRect(activeFinding.maskRect, page.width, page.height)}
                  onClick={() => void saveCoordinates()}
                >
                  保存坐标
                </button>
              </div>
            )}
          </aside>
        </div>

        <footer className="review-footer">
          <span>
            {pendingGroups > 0
              ? `当前${embeddedImage ? "图片" : "页面"}还有 ${pendingGroups} 个结果需要确认`
              : !canExport
                ? "文档其他内容仍有待确认"
                : "所有结果已复核，可以执行安全导出"}
          </span>
          <button
            className="primary-button"
            type="button"
            disabled={busy || exporting || !canExport}
            onClick={() => void onExport()}
          >
            {exporting ? "正在导出并复检…" : "保存安全副本"}
            <ChevronRightIcon />
          </button>
        </footer>
      </section>
    </div>
  );
}
