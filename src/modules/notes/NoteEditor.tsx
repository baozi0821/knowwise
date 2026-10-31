// 笔记编辑器:格式工具栏(粗体/斜体/标题/列表/任务清单/表格/代码)+
// 手绘与几何形状(自由/矩形/椭圆/直线/箭头,以 kwink 围栏嵌入)+
// 屏幕截图(Ctrl+Z/Ctrl+Shift+Z 撤销重做)+ 剪贴板截图/贴图 + 编辑实时预览。
// 「乱码」治理:kwink JSON 与 data-URI 图片在源码框中折叠为
// 〔手绘图形〕/〔图片〕占位符,保存时自动还原,编辑器只见可读文本。
import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import {
  Bold,
  Italic,
  Heading2,
  List,
  ListChecks,
  Table,
  Code2,
  PenLine,
  Square,
  Circle,
  Minus,
  MoveUpRight,
  Spline,
  Trash2,
  Check,
  X,
  ClipboardPaste,
  Eye,
  Camera,
  Undo2,
  Redo2,
  Paperclip,
} from "lucide-react";
import { encodeInk, type InkShapeType, type InkStroke } from "../reader/annotations";
import { inkStrokeElements } from "../reader/InkSvg";
import { Markdown } from "../reader/Markdown";
import { PEN_COLORS } from "../reader/readerTools";
import { useScreenClip } from "../capture/screenClip";
import { createUndoStack } from "./undoStack";

/** 在光标处包裹/插入片段;返回新文本与新光标位置。 */
export function insertSnippet(
  text: string,
  start: number,
  end: number,
  prefix: string,
  suffix = "",
  placeholder = "",
): { text: string; cursor: number } {
  const selected = text.slice(start, end) || placeholder;
  const next = text.slice(0, start) + prefix + selected + suffix + text.slice(end);
  return { text: next, cursor: start + prefix.length + selected.length };
}


const KWINK_PLACEHOLDER = "〔手绘图形〕";
const IMAGE_PLACEHOLDER = "〔图片〕";

/** 把源码中的长数据块(kwink 围栏、裸笔迹串、data-URI 图片)折叠为占位符。 */
export function maskLongBlocks(source: string): {
  masked: string;
  originals: string[];
} {
  const originals: string[] = [];
  const masked = source
    .replace(/```kwink\n[\s\S]*?\n```/g, (m) => {
      originals.push(m);
      return KWINK_PLACEHOLDER;
    })
    .replace(/ink:v1:\S+/g, (m) => {
      originals.push(m);
      return KWINK_PLACEHOLDER;
    })
    .replace(/!\[[^\]]*\]\(data:[^)]+\)/g, (m) => {
      originals.push(m);
      return IMAGE_PLACEHOLDER;
    });
  return { masked, originals };
}

/** 占位符还原为真实数据块;被删除的占位符对应数据一并丢弃。 */
export function unmaskLongBlocks(masked: string, originals: string[]): string {
  let index = 0;
  const pattern = new RegExp(`${KWINK_PLACEHOLDER}|${IMAGE_PLACEHOLDER}`, "g");
  return masked.replace(pattern, (m) => {
    if (index >= originals.length) return m;
    return originals[index++]!;
  });
}

type SketchTool = "free" | InkShapeType;

const SKETCH_TOOLS: Array<{ id: SketchTool; label: string; icon: typeof Spline }> = [
  { id: "free", label: "自由笔", icon: Spline },
  { id: "rect", label: "矩形", icon: Square },
  { id: "ellipse", label: "椭圆", icon: Circle },
  { id: "line", label: "直线", icon: Minus },
  { id: "arrow", label: "箭头", icon: MoveUpRight },
];

/** 手绘/图形对话框:确定后以 ```kwink 围栏插入正文。 */
function SketchDialog({
  onInsert,
  onClose,
}: {
  onInsert: (fence: string) => void;
  onClose: () => void;
}): ReactNode {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const currentRef = useRef<InkStroke | null>(null);
  const [committed, setCommitted] = useState<InkStroke[]>([]);
  const [color, setColor] = useState(PEN_COLORS[0]!);
  const [tool, setTool] = useState<SketchTool>("free");
  const W = 600;
  const H = 220;

  const redraw = useCallback((all: InkStroke[]) => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (canvas == null || ctx == null) return;
    ctx.fillStyle = "#fffdf5";
    ctx.fillRect(0, 0, W, H);
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    for (const s of all) {
      ctx.strokeStyle = s.color;
      ctx.fillStyle = s.color;
      ctx.lineWidth = Math.max(2, s.width * W);
      const a = s.points[0]!;
      const b = s.points[s.points.length - 1]!;
      const x1 = a.x * W;
      const y1 = a.y * H;
      const x2 = b.x * W;
      const y2 = b.y * H;
      if (s.t === undefined) {
        ctx.beginPath();
        s.points.forEach((p, i) => {
          if (i === 0) ctx.moveTo(p.x * W, p.y * H);
          else ctx.lineTo(p.x * W, p.y * H);
        });
        ctx.stroke();
        continue;
      }
      if (s.t === "rect") {
        ctx.strokeRect(Math.min(x1, x2), Math.min(y1, y2), Math.abs(x2 - x1), Math.abs(y2 - y1));
      } else if (s.t === "ellipse") {
        ctx.beginPath();
        ctx.ellipse(
          (x1 + x2) / 2,
          (y1 + y2) / 2,
          Math.abs(x2 - x1) / 2,
          Math.abs(y2 - y1) / 2,
          0,
          0,
          Math.PI * 2,
        );
        ctx.stroke();
      } else if (s.t === "line") {
        ctx.beginPath();
        ctx.moveTo(x1, y1);
        ctx.lineTo(x2, y2);
        ctx.stroke();
      } else if (s.t === "arrow") {
        ctx.beginPath();
        ctx.moveTo(x1, y1);
        ctx.lineTo(x2, y2);
        ctx.stroke();
        const angle = Math.atan2(y2 - y1, x2 - x1);
        const head = 12;
        ctx.beginPath();
        ctx.moveTo(x2, y2);
        ctx.lineTo(
          x2 - head * Math.cos(angle - Math.PI / 7),
          y2 - head * Math.sin(angle - Math.PI / 7),
        );
        ctx.moveTo(x2, y2);
        ctx.lineTo(
          x2 - head * Math.cos(angle + Math.PI / 7),
          y2 - head * Math.sin(angle + Math.PI / 7),
        );
        ctx.stroke();
      }
    }
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (canvas === null) return;
    canvas.width = W;
    canvas.height = H;
    redraw(committed);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    redraw(committed);
  }, [committed, redraw]);

  const pointer = (event: React.PointerEvent<HTMLCanvasElement>): { x: number; y: number } => {
    const rect = event.currentTarget.getBoundingClientRect();
    return {
      x: Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width)),
      y: Math.min(1, Math.max(0, (event.clientY - rect.top) / rect.height)),
    };
  };

  return (
    <div className="sketch-overlay" role="dialog" aria-label="插入手绘或图形">
      <div className="sketch-dialog">
        <div className="sketch-toolbar">
          <div className="sketch-tool-group" role="radiogroup" aria-label="绘制工具">
            {SKETCH_TOOLS.map((t) => {
              const Icon = t.icon;
              return (
                <button
                  key={t.id}
                  type="button"
                  className={`icon-btn small${tool === t.id ? " active" : ""}`}
                  title={t.label}
                  aria-label={t.label}
                  aria-pressed={tool === t.id}
                  onClick={() => setTool(t.id)}
                >
                  <Icon className="w-3.5 h-3.5" />
                </button>
              );
            })}
          </div>
          <span className="pen-colors">
            {PEN_COLORS.map((c) => (
              <button
                key={c}
                type="button"
                className={`color-dot${color === c ? " selected" : ""}`}
                style={{ backgroundColor: c }}
                aria-label={`颜色 ${c}`}
                onClick={() => setColor(c)}
              />
            ))}
          </span>
          <button type="button" className="btn small ghost" onClick={() => setCommitted([])}>
            <Trash2 className="w-3.5 h-3.5" aria-hidden />
            清空
          </button>
          <span className="sketch-spacer" />
          <button type="button" className="btn small ghost" onClick={onClose}>
            <X className="w-3.5 h-3.5" aria-hidden />
            取消
          </button>
          <button
            type="button"
            className="btn small primary"
            disabled={committed.length === 0}
            onClick={() => {
              onInsert(`\n\`\`\`kwink\n${encodeInk(committed).replace(/^ink:v1:/, "")}\n\`\`\`\n`);
            }}
          >
            <Check className="w-3.5 h-3.5" aria-hidden />
            插入
          </button>
        </div>
        <canvas
          ref={canvasRef}
          className="sketch-canvas"
          onPointerDown={(event) => {
            event.currentTarget.setPointerCapture?.(event.pointerId);
            const p = pointer(event);
            currentRef.current = {
              color,
              width: 0.0035,
              points: [p],
              t: tool === "free" ? undefined : tool,
            };
          }}
          onPointerMove={(event) => {
            const cur = currentRef.current;
            if (cur === null || !(event.buttons & 1)) return;
            const p = pointer(event);
            if (cur.t === undefined) {
              cur.points.push(p);
            } else {
              // 形状:首点固定,末点随拖动(橡皮筋)。
              cur.points[1] = p;
            }
            redraw([...committed, cur]);
          }}
          onPointerUp={() => {
            const cur = currentRef.current;
            currentRef.current = null;
            if (cur === null) return;
            if (cur.points.length === 1) cur.points.push({ ...cur.points[0]! });
            setCommitted((prev) => [...prev, cur]);
          }}
        />
      </div>
    </div>
  );
}

/** 剪贴板图片 → 紧凑 dataURI(找不到图片返回 null)。 */
async function clipboardImageToDataUrl(): Promise<string | null> {
  try {
    const items = await navigator.clipboard.read();
    for (const item of items) {
      const type = item.types.find((t) => t.startsWith("image/"));
      if (type === undefined) continue;
      const blob = await item.getType(type);
      return blobToCompactDataUrl(blob);
    }
  } catch {
    // 无权限或不支持:走提示。
  }
  return null;
}

export async function blobToCompactDataUrl(file: Blob): Promise<string | null> {
  const bitmap = await createImageBitmap(file).catch(() => null);
  if (bitmap === null) return null;
  const budgetChars = 60_000;
  for (const maxWidth of [560, 420, 320, 240]) {
    for (const quality of [0.72, 0.55, 0.4]) {
      const scale = Math.min(1, maxWidth / bitmap.width);
      const w = Math.max(1, Math.round(bitmap.width * scale));
      const h = Math.max(1, Math.round(bitmap.height * scale));
      const canvas = document.createElement("canvas");
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext("2d");
      if (ctx === null) return null;
      ctx.drawImage(bitmap, 0, 0, w, h);
      const url = canvas.toDataURL("image/jpeg", quality);
      if (url.length <= budgetChars) return url;
    }
  }
  return null;
}


/** 可视化表格编辑:先填格子再插入 Markdown,不要求手写竖线。 */
function TableDialog({
  onInsert,
  onClose,
}: {
  onInsert: (markdown: string) => void;
  onClose: () => void;
}): ReactNode {
  const [rows, setRows] = useState(3);
  const [cols, setCols] = useState(3);
  const [cells, setCells] = useState<string[][]>(() =>
    Array.from({ length: 4 }, (_, r) =>
      Array.from({ length: 4 }, (_, c) => (r === 0 ? `列 ${c + 1}` : "")),
    ),
  );
  const setCell = (r: number, c: number, value: string) =>
    setCells((prev) => {
      const next = prev.map((row) => [...row]);
      next[r]![c] = value.replace(/\|/g, "\\|");
      return next;
    });
  const insert = () => {
    const header = Array.from({ length: cols }, (_, c) => cells[0]![c] || `列 ${c + 1}`);
    const lines = [
      `| ${header.join(" | ")} |`,
      `| ${Array.from({ length: cols }, () => "---").join(" | ")} |`,
    ];
    for (let r = 1; r < rows; r += 1) {
      lines.push(`| ${Array.from({ length: cols }, (_, c) => cells[r]![c] ?? "").join(" | ")} |`);
    }
    onInsert(`\n${lines.join("\n")}\n`);
  };
  return (
    <div className="sketch-overlay" role="dialog" aria-label="可视化表格">
      <div className="sketch-dialog table-dialog">
        <div className="sketch-toolbar">
          <strong>可视化表格</strong>
          <label className="table-size">
            行
            <select
              aria-label="表格行数"
              value={rows}
              onChange={(event) => setRows(Math.min(12, Math.max(2, Number(event.target.value))))}
            >
              {Array.from({ length: 11 }, (_, i) => i + 2).map((n) => (
                <option key={n} value={n}>
                  {n}
                </option>
              ))}
            </select>
          </label>
          <label className="table-size">
            列
            <select
              aria-label="表格列数"
              value={cols}
              onChange={(event) => setCols(Math.min(8, Math.max(2, Number(event.target.value))))}
            >
              {Array.from({ length: 7 }, (_, i) => i + 2).map((n) => (
                <option key={n} value={n}>
                  {n}
                </option>
              ))}
            </select>
          </label>
          <span className="sketch-spacer" />
          <button type="button" className="btn small ghost" onClick={onClose}>
            取消
          </button>
          <button type="button" className="btn small primary" onClick={insert}>
            插入表格
          </button>
        </div>
        <div className="table-grid" style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))` }}>
          {Array.from({ length: rows }, (_, r) =>
            Array.from({ length: cols }, (_, c) => (
              <input
                key={`${r}-${c}`}
                value={cells[r]?.[c] ?? ""}
                aria-label={`第 ${r + 1} 行第 ${c + 1} 列`}
                placeholder={r === 0 ? "表头" : ""}
                onChange={(event) => setCell(r, c, event.target.value)}
              />
            )),
          )}
        </div>
      </div>
    </div>
  );
}

export function NoteEditor({
  value,
  onChange,
  ariaLabel,
  placeholder,
  maxLength = 2000,
  rows = 8,
}: {
  value: string;
  onChange: (next: string) => void;
  ariaLabel: string;
  placeholder?: string;
  maxLength?: number;
  rows?: number;
}): ReactNode {
  const ref = useRef<HTMLTextAreaElement>(null);
  const [sketchOpen, setSketchOpen] = useState(false);
  const [tableOpen, setTableOpen] = useState(false);
  const [slashOpen, setSlashOpen] = useState(false);
  const [hint, setHint] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [preview, setPreview] = useState(true);
  /** 编辑/预览竖向六四开;拖拽分隔条可调(25%–80%),按用户记忆。 */
  const [splitPercent, setSplitPercent] = useState<number>(() => {
    try {
      const saved = Number(window.localStorage.getItem("kw.notes.split.v1"));
      return Number.isFinite(saved) && saved >= 25 && saved <= 80 ? saved : 60;
    } catch {
      return 60;
    }
  });
  const splitRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ startY: number; startPercent: number; height: number } | null>(null);
  /** 撤销栈推进后触发重渲染(栈本身放在 ref 中,避免每次输入重建)。 */
  const [, forceHistoryRender] = useState(0);
  const stackRef = useRef(createUndoStack(value));
  const clip = useScreenClip();
  const { masked, originals } = useMemo(() => maskLongBlocks(value), [value]);

  const showHint = (message: string, ms = 3000) => {
    setHint(message);
    setTimeout(() => setHint(null), ms);
  };

  // 外部改动(例如「更多 → 截图插入」)也纳入撤销历史。
  useEffect(() => {
    if (value !== stackRef.current.current()) {
      stackRef.current.push(value);
      forceHistoryRender((tick) => tick + 1);
    }
  }, [value]);

  const restoreCursor = (position: number) => {
    requestAnimationFrame(() => {
      const el = ref.current;
      if (el === null) return;
      el.focus();
      el.setSelectionRange(position, position);
    });
  };

  const commit = (next: string) => {
    if (next === stackRef.current.current()) return;
    stackRef.current.push(next);
    forceHistoryRender((tick) => tick + 1);
    onChange(next);
    if (next.length > maxLength) {
      showHint(`内容已超过 ${maxLength} 字上限,请精简后再保存`, 4000);
    }
  };

  const apply = (prefix: string, suffix = "", snippet = "") => {
    const el = ref.current;
    const start = el?.selectionStart ?? masked.length;
    const end = el?.selectionEnd ?? masked.length;
    const { text, cursor } = insertSnippet(masked, start, end, prefix, suffix, snippet);
    commit(unmaskLongBlocks(text, originals));
    restoreCursor(cursor);
  };

  const undo = () => {
    const previous = stackRef.current.undo();
    if (previous === null) return;
    forceHistoryRender((tick) => tick + 1);
    onChange(previous);
    restoreCursor(previous.length);
  };

  const redo = () => {
    const next = stackRef.current.redo();
    if (next === null) return;
    forceHistoryRender((tick) => tick + 1);
    onChange(next);
    restoreCursor(next.length);
  };

  /** 在光标处插入整段片段;若刚输入的 "/" 触发命令,先把 "/" 去掉。 */
  const insertCommand = (snippet: string, placeholder = "") => {
    const el = ref.current;
    const start = el?.selectionStart ?? masked.length;
    const end = el?.selectionEnd ?? masked.length;
    const before = masked.slice(0, start);
    const cleaned = /(^|\s)\/$/.test(before) ? before.slice(0, -1) : before;
    const selected = masked.slice(start, end) || placeholder;
    const marker = snippet.indexOf("$SEL");
    const inserted = snippet.replace("$SEL", selected);
    const next = `${cleaned}${inserted}${masked.slice(end)}`;
    setSlashOpen(false);
    commit(unmaskLongBlocks(next, originals));
    restoreCursor(
      marker >= 0
        ? cleaned.length + marker + selected.length
        : cleaned.length + inserted.length,
    );
  };

  const insertImages = (files: File[]) => {
    void (async () => {
      for (const file of files) {
        if (!file.type.startsWith("image/")) continue;
        const url = await blobToCompactDataUrl(file);
        if (url === null) {
          showHint(`图片「${file.name}」压缩后仍超限,未插入`, 3000);
          continue;
        }
        insertImage(url);
      }
    })();
  };

  const insertImage = (url: string) => {
    // 标注正文上限 2000 字、普通笔记 100000 字;data-URI 远超标注上限,
    // 提前给出可执行建议,而不是插入后保存失败(不静默、不丢用户操作)。
    if (maxLength < 20_000 || url.length + value.length > maxLength) {
      showHint(
        `当前条目正文上限 ${maxLength} 字,放不下这张截图;请点顶部「新建」创建普通笔记后再截图插入`,
        5000,
      );
      return;
    }
    apply(`\n![截图](${url})\n`);
  };

  const insertScreenClip = () => {
    if (!clip.supported) {
      showHint("当前环境不支持屏幕截图(仅桌面应用可用)");
      return;
    }
    if (maxLength < 20_000) {
      showHint(
        `标注正文上限 ${maxLength} 字,存放不了截图;请先点「新建」创建一条普通笔记,再在笔记里截图插入`,
        5000,
      );
      return;
    }
    void (async () => {
      const url = await clip.requestClip();
      if (url === null) {
        showHint("已取消截图");
        return;
      }
      insertImage(url);
    })();
  };

  const canUndo = stackRef.current.canUndo();
  const canRedo = stackRef.current.canRedo();

  const startSplitDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    const box = splitRef.current;
    if (box === null) return;
    event.preventDefault();
    (event.target as HTMLElement).setPointerCapture?.(event.pointerId);
    dragRef.current = {
      startY: event.clientY,
      startPercent: splitPercent,
      height: Math.max(1, box.getBoundingClientRect().height),
    };
    const move = (e: PointerEvent) => {
      const drag = dragRef.current;
      if (drag === null) return;
      const next = Math.min(
        80,
        Math.max(25, drag.startPercent + ((e.clientY - drag.startY) / drag.height) * 100),
      );
      setSplitPercent(Math.round(next));
    };
    const up = () => {
      dragRef.current = null;
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      setSplitPercent((value) => {
        try {
          window.localStorage.setItem("kw.notes.split.v1", String(value));
        } catch {
          // 忽略存储不可用。
        }
        return value;
      });
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up, { once: true });
  };

  return (
    <div className="note-editor">
      <div className="note-editor-toolbar" role="toolbar" aria-label="格式">
        <button
          type="button"
          className="icon-btn small"
          title="撤销(Ctrl+Z)"
          aria-label="撤销"
          disabled={!canUndo}
          onClick={undo}
        >
          <Undo2 className="w-3.5 h-3.5" />
        </button>
        <button
          type="button"
          className="icon-btn small"
          title="重做(Ctrl+Shift+Z)"
          aria-label="重做"
          disabled={!canRedo}
          onClick={redo}
        >
          <Redo2 className="w-3.5 h-3.5" />
        </button>
        <span className="note-editor-divider" aria-hidden />
        <button type="button" className="icon-btn small" title="粗体" aria-label="粗体" onClick={() => apply("**", "**", "粗体")}>
          <Bold className="w-3.5 h-3.5" />
        </button>
        <button type="button" className="icon-btn small" title="斜体" aria-label="斜体" onClick={() => apply("*", "*", "斜体")}>
          <Italic className="w-3.5 h-3.5" />
        </button>
        <button type="button" className="icon-btn small" title="标题" aria-label="标题" onClick={() => apply("\n## ", "", "标题")}>
          <Heading2 className="w-3.5 h-3.5" />
        </button>
        <button type="button" className="icon-btn small" title="列表" aria-label="列表" onClick={() => apply("\n- ", "", "条目")}>
          <List className="w-3.5 h-3.5" />
        </button>
        <button type="button" className="icon-btn small" title="任务清单" aria-label="任务清单" onClick={() => apply("\n- [ ] ", "", "待办")}>
          <ListChecks className="w-3.5 h-3.5" />
        </button>
        <button type="button" className="icon-btn small" title="可视化表格" aria-label="插入表格" onClick={() => setTableOpen(true)}>
          <Table className="w-3.5 h-3.5" />
        </button>
        <button
          type="button"
          className="icon-btn small"
          title="斜杠命令"
          aria-label="斜杠命令"
          aria-pressed={slashOpen}
          onClick={() => setSlashOpen((open) => !open)}
        >
          <span aria-hidden="true">/</span>
        </button>
        <button
          type="button"
          className="icon-btn small"
          title="插入图片附件"
          aria-label="插入图片附件"
          onClick={() => fileInputRef.current?.click()}
        >
          <Paperclip className="w-3.5 h-3.5" />
        </button>
        <input
          ref={fileInputRef}
          type="file"
          accept="image/*"
          multiple
          hidden
          aria-label="选择图片附件"
          onChange={(event) => {
            const files = Array.from(event.target.files ?? []);
            event.target.value = "";
            insertImages(files);
          }}
        />
        <button type="button" className="icon-btn small" title="代码块" aria-label="代码块" onClick={() => apply("\n```\n", "\n```\n", "代码")}>
          <Code2 className="w-3.5 h-3.5" />
        </button>
        <button type="button" className="icon-btn small" title="手绘/几何图形" aria-label="插入手绘或图形" onClick={() => setSketchOpen(true)}>
          <PenLine className="w-3.5 h-3.5" />
        </button>
        <button
          type="button"
          className="icon-btn small"
          title={
            clip.supported && maxLength >= 20_000
              ? "框选屏幕区域截图并插入(Ctrl+V 也可粘贴图片)"
              : maxLength < 20_000
                ? "标注正文上限 2000 字,截图请存到「新建」的普通笔记"
                : "当前环境不支持屏幕截图(仅桌面应用可用)"
          }
          aria-label="屏幕截图"
          disabled={!clip.supported || maxLength < 20_000}
          onClick={insertScreenClip}
        >
          <Camera className="w-3.5 h-3.5" />
        </button>
        <button
          type="button"
          className="icon-btn small"
          title="粘贴截图/图片(也可直接 Ctrl+V)"
          aria-label="粘贴截图"
          onClick={() => {
            void (async () => {
              const url = await clipboardImageToDataUrl();
              if (url === null) {
                showHint("剪贴板没有图片;用 Win+Shift+S 截图后重试,或直接 Ctrl+V", 3500);
                return;
              }
              insertImage(url);
            })();
          }}
        >
          <ClipboardPaste className="w-3.5 h-3.5" />
        </button>
        <span className="note-editor-spacer" />
        <button
          type="button"
          className={`icon-btn small${preview ? " active" : ""}`}
          title={preview ? "隐藏预览" : "显示实时预览"}
          aria-label={preview ? "隐藏预览" : "显示实时预览"}
          aria-pressed={preview}
          onClick={() => setPreview((v) => !v)}
        >
          <Eye className="w-3.5 h-3.5" />
        </button>
      </div>
      <div
        ref={splitRef}
        className={preview ? "notes-workspace-split" : "notes-workspace-single"}
        style={preview ? { gridTemplateRows: `${splitPercent}fr 7px ${100 - splitPercent}fr` } : undefined}
      >
      <textarea
        ref={ref}
        value={masked}
        rows={rows}
        aria-label={ariaLabel}
        placeholder={placeholder ?? "支持 Markdown、手绘图形、表格、图片(〔…〕为嵌入内容)"}
        onChange={(e) => {
          const next = e.target.value;
          commit(unmaskLongBlocks(next, originals));
          const caret = e.target.selectionStart ?? next.length;
          const before = next.slice(0, caret);
          setSlashOpen(/(^|\s)\/$/.test(before) && next.length <= masked.length + 1);
        }}
        onDragOver={(event) => {
          if (event.dataTransfer?.types.includes("Files")) event.preventDefault();
        }}
        onDrop={(event) => {
          const files = Array.from(event.dataTransfer?.files ?? []);
          if (files.some((file) => file.type.startsWith("image/"))) {
            event.preventDefault();
            insertImages(files);
          }
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape" && slashOpen) {
            event.stopPropagation();
            setSlashOpen(false);
            return;
          }
          const modifier = event.ctrlKey || event.metaKey;
          if (!modifier || event.altKey) return;
          const key = event.key.toLowerCase();
          if (key === "z") {
            event.preventDefault();
            if (event.shiftKey) redo();
            else undo();
            return;
          }
          if (key === "y") {
            event.preventDefault();
            redo();
          }
        }}
        onPaste={(event) => {
          const items = event.clipboardData?.items;
          if (items === undefined) return;
          for (const item of items) {
            if (item.type.startsWith("image/")) {
              const file = item.getAsFile();
              if (file === null) continue;
              event.preventDefault();
              void (async () => {
                const url = await blobToCompactDataUrl(file);
                if (url === null) {
                  showHint("图片过大,压缩后仍超限,未插入", 2500);
                  return;
                }
                insertImage(url);
              })();
              return;
            }
          }
        }}
      />
      {preview ? (
        <>
          <div
            className="notes-split-bar"
            role="separator"
            aria-orientation="horizontal"
            aria-label="调整编辑/预览高度"
            aria-valuemin={25}
            aria-valuemax={80}
            aria-valuenow={splitPercent}
            tabIndex={0}
            title="拖动调整编辑/预览比例(双击恢复 6:4)"
            onPointerDown={startSplitDrag}
            onDoubleClick={() => setSplitPercent(60)}
            onKeyDown={(event) => {
              if (event.key === "ArrowUp") setSplitPercent((v) => Math.max(25, v - 5));
              if (event.key === "ArrowDown") setSplitPercent((v) => Math.min(80, v + 5));
            }}
          >
            <span aria-hidden="true" />
          </div>
          <div className="note-editor-preview note-md" aria-label="实时预览">
            <Markdown>{value}</Markdown>
          </div>
        </>
      ) : null}
      </div>
      {hint ? <p className="note-editor-hint" role="status">{hint}</p> : null}
      {slashOpen ? (
        <div className="note-slash-menu" role="menu" aria-label="斜杠命令">
          {[
            { label: "标题", snippet: "## $SEL", placeholder: "标题" },
            { label: "列表", snippet: "- $SEL", placeholder: "条目" },
            { label: "任务清单", snippet: "- [ ] $SEL", placeholder: "待办" },
            { label: "代码块", snippet: "```\n$SEL\n```", placeholder: "代码" },
            { label: "表格", action: () => setTableOpen(true) },
            { label: "手绘", action: () => setSketchOpen(true) },
            { label: "图片", action: () => fileInputRef.current?.click() },
          ].map((item) => (
            <button
              key={item.label}
              type="button"
              role="menuitem"
              onClick={() => {
                if (item.action) {
                  setSlashOpen(false);
                  item.action();
                } else {
                  insertCommand(item.snippet!, item.placeholder);
                }
              }}
            >
              {item.label}
            </button>
          ))}
        </div>
      ) : null}
      {tableOpen ? (
        <TableDialog
          onClose={() => setTableOpen(false)}
          onInsert={(markdown) => {
            setTableOpen(false);
            apply(markdown);
          }}
        />
      ) : null}
      {sketchOpen ? (
        <SketchDialog
          onClose={() => setSketchOpen(false)}
          onInsert={(fence) => {
            setSketchOpen(false);
            apply(fence);
          }}
        />
      ) : null}
      <div className="note-editor-row">
        <span className={`note-count-hint${value.length > maxLength ? " over" : ""}`}>
          {value.length}/{maxLength}
        </span>
      </div>
    </div>
  );
}

/** 供测试与外部预览使用:渲染一组笔迹(含几何形状)。 */
export function InkPreview({ strokes }: { strokes: InkStroke[] }): ReactNode {
  return (
    <svg viewBox="0 0 600 220" width={600} height={220}>
      {inkStrokeElements(strokes, 600, 220)}
    </svg>
  );
}
