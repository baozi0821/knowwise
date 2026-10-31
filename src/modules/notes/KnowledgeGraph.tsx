// 知识图:只使用真实数据构造节点与边(文档、章节、标注、笔记、问答引用、书签)。
// 设计语义:
// - 节点按类型区分形状+颜色+文字;边只画真实存在的「包含/位于/引用/相关」,
//   支持/冲突没有可靠数据时不生成(图例明确说明,不推断)。
// - 默认完整图、文档居中;当前阅读位置附近节点高亮,可只看邻域。
// - 点击选中并在详情侧栏显示来源/页码/摘录与「回到原文」;双击聚焦邻居。
// - 节点超限(200)按类型优先级截断并明确告知;确定性布局,无随机抖动。
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { useQuery } from "@tanstack/react-query";
import {
  Crosshair,
  Focus,
  Maximize2,
  Minimize2,
  RotateCcw,
  Search,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import type { CommandClient } from "../../app/api/client";
import type { BookmarkView } from "../../shared/generated/BookmarkView";
import type { DocumentDetail } from "../../shared/generated/DocumentDetail";
import type { MessageContent } from "../../shared/generated/MessageContent";
import type { MessageView } from "../../shared/generated/MessageView";
import type { Page } from "../../shared/generated/Page";
import type { SourceLocator } from "../../shared/generated/SourceLocator";
import type { NoteItem } from "./noteItems";
import { jumpToLocator } from "./jumpToLocator";
import { useReaderTools } from "../reader/readerTools";
import { resolveCitationAndGoto } from "../qa/AnswerView";
import { useWorkspaceUi } from "../workspace/uiStore";

type NodeKind = "document" | "section" | "annotation" | "note" | "bookmark" | "citation";
type EdgeKind = "contains" | "located" | "citation" | "related";
type LayoutMode = "hierarchy" | "force";

type GraphNode = {
  id: string;
  kind: NodeKind;
  label: string;
  detail?: string;
  excerpt?: string;
  locator?: SourceLocator | null;
  annotationId?: string;
  noteId?: string;
  bookmark?: BookmarkView;
  citationId?: string;
  x?: number;
  y?: number;
};

type GraphEdge = { from: string; to: string; kind: EdgeKind };

const NODE_LABELS: Record<NodeKind, string> = {
  document: "文档",
  section: "章节",
  annotation: "标注",
  note: "笔记",
  bookmark: "书签",
  citation: "问答引用",
};

const EDGE_LABELS: Record<EdgeKind, string> = {
  contains: "包含",
  located: "位于",
  citation: "引用",
  related: "相关",
};

const MAX_NODES = 200;
const VIEW_W = 680;
const VIEW_H = 500;
const MIN_ZOOM = 0.45;
const MAX_ZOOM = 3;

function clamp(value: number, lo: number, hi: number): number {
  return Math.min(Math.max(value, lo), hi);
}

function sectionName(locator: SourceLocator | null | undefined): string | null {
  if (locator === null || locator === undefined) return null;
  if (locator.kind === "markdown" || locator.kind === "docx") {
    const path = locator.headingPath;
    return path.length > 0 ? path[path.length - 1]! : null;
  }
  if (locator.kind === "composite") {
    for (const part of locator.parts) {
      const name = sectionName(part);
      if (name !== null) return name;
    }
  }
  return null;
}

function locatorPage(locator: SourceLocator | null | undefined): number | null {
  if (locator === null || locator === undefined) return null;
  if (locator.kind === "pdf") return locator.pageNumber;
  if (locator.kind === "composite") {
    for (const part of locator.parts) {
      const page = locatorPage(part);
      if (page !== null) return page;
    }
  }
  return null;
}

function headingPathOf(locator: SourceLocator | null | undefined): string[] {
  if (locator === null || locator === undefined) return [];
  if (locator.kind === "markdown" || locator.kind === "docx") return locator.headingPath;
  if (locator.kind === "composite") {
    for (const part of locator.parts) {
      const path = headingPathOf(part);
      if (path.length > 0) return path;
    }
  }
  return [];
}

function clip(text: string, max: number): string {
  const trimmed = text.trim();
  return trimmed.length <= max ? trimmed : `${trimmed.slice(0, Math.max(1, max - 1))}…`;
}

/** 层级布局:文档居中,章节内环,其余按类型分扇区排在外环。 */
function layoutHierarchy(nodes: GraphNode[]): void {
  const centerX = VIEW_W / 2;
  const centerY = VIEW_H / 2 - 10;
  const outerKinds: NodeKind[] = ["annotation", "note", "bookmark", "citation"];
  outerKinds.forEach((kind, kindIndex) => {
    const sameKind = nodes.filter((node) => node.kind === kind);
    sameKind.forEach((node, index) => {
      const base = -Math.PI / 2 + (kindIndex * Math.PI) / 2;
      const spread = Math.PI / 2.6;
      const angle =
        sameKind.length === 1 ? base : base + ((index + 0.5) / sameKind.length - 0.5) * spread;
      node.x = centerX + Math.cos(angle) * 218;
      node.y = centerY + Math.sin(angle) * 182;
    });
  });
  const sections = nodes.filter((node) => node.kind === "section");
  sections.forEach((node, index) => {
    const angle = (index / Math.max(1, sections.length)) * Math.PI * 2 - Math.PI / 2;
    node.x = centerX + Math.cos(angle) * 118;
    node.y = centerY + Math.sin(angle) * 104;
  });
  const doc = nodes.find((node) => node.kind === "document");
  if (doc !== undefined) {
    doc.x = centerX;
    doc.y = centerY;
  }
}

/** 确定性力导向(Fruchterman–Reingold 变体):固定初值,有限迭代,无随机。 */
function layoutForce(nodes: GraphNode[], edges: GraphEdge[]): void {
  const count = nodes.length;
  if (count === 0) return;
  const indexById = new Map(nodes.map((node, index) => [node.id, index]));
  const positions = nodes.map((_, index) => ({
    x: VIEW_W / 2 + Math.cos(index * 2.399963) * (140 + (index % 7) * 12),
    y: VIEW_H / 2 + Math.sin(index * 2.399963) * (120 + (index % 5) * 12),
  }));
  const area = VIEW_W * VIEW_H;
  const k = Math.sqrt(area / count) * 0.62;
  const iterations = count > 120 ? 120 : 180;
  for (let iteration = 0; iteration < iterations; iteration += 1) {
    const dispX = new Array<number>(count).fill(0);
    const dispY = new Array<number>(count).fill(0);
    for (let i = 0; i < count; i += 1) {
      for (let j = i + 1; j < count; j += 1) {
        let dx = positions[i]!.x - positions[j]!.x;
        let dy = positions[i]!.y - positions[j]!.y;
        let distance = Math.hypot(dx, dy);
        if (distance < 0.5) {
          dx = (i % 3) - 1 + 0.5;
          dy = (j % 3) - 1 + 0.5;
          distance = Math.hypot(dx, dy) || 1;
        }
        const force = (k * k) / distance;
        const ux = dx / distance;
        const uy = dy / distance;
        dispX[i]! += ux * force;
        dispY[i]! += uy * force;
        dispX[j]! -= ux * force;
        dispY[j]! -= uy * force;
      }
    }
    for (const edge of edges) {
      const a = indexById.get(edge.from);
      const b = indexById.get(edge.to);
      if (a === undefined || b === undefined) continue;
      const dx = positions[a]!.x - positions[b]!.x;
      const dy = positions[a]!.y - positions[b]!.y;
      const distance = Math.hypot(dx, dy) || 1;
      const force = (distance * distance) / k;
      const ux = dx / distance;
      const uy = dy / distance;
      dispX[a]! -= ux * force;
      dispY[a]! -= uy * force;
      dispX[b]! += ux * force;
      dispY[b]! += uy * force;
    }
    const temperature = (VIEW_W / 9) * (1 - iteration / iterations) + 1;
    for (let index = 0; index < count; index += 1) {
      const dx = dispX[index]!;
      const dy = dispY[index]!;
      const distance = Math.hypot(dx, dy) || 1;
      const step = Math.min(distance, temperature);
      positions[index]!.x += (dx / distance) * step;
      positions[index]!.y += (dy / distance) * step;
      positions[index]!.x = clamp(positions[index]!.x, 34, VIEW_W - 34);
      positions[index]!.y = clamp(positions[index]!.y, 34, VIEW_H - 34);
    }
  }
  nodes.forEach((node, index) => {
    node.x = positions[index]!.x;
    node.y = positions[index]!.y;
  });
}

export function KnowledgeGraph({
  documentId,
  client,
  items,
  bookmarks,
  conversationId,
  onToast,
}: {
  documentId: string;
  client: CommandClient;
  items: NoteItem[];
  bookmarks: BookmarkView[];
  conversationId: string | null;
  onToast: (kind: "success" | "error", message: string) => void;
}): ReactNode {
  const [hidden, setHidden] = useState<Set<NodeKind>>(() => new Set());
  const [activeId, setActiveId] = useState<string | null>(null);
  const [focusId, setFocusId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [layoutMode, setLayoutMode] = useState<LayoutMode>("hierarchy");
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [fullscreen, setFullscreen] = useState(false);
  const [readingNearOnly, setReadingNearOnly] = useState(false);
  const svgRef = useRef<SVGSVGElement | null>(null);

  const viewport = useWorkspaceUi((state) => state.readerViewport[documentId]);
  const detail = useQuery({
    queryKey: ["document", "graph", documentId],
    queryFn: () => client.call<DocumentDetail>("document_get", { documentId }),
    staleTime: 60_000,
  });
  const messages = useQuery({
    queryKey: ["conversation", "graph", conversationId ?? "none"],
    queryFn: () =>
      client.call<Page<MessageView>>("conversation_get_messages", {
        conversationId: conversationId!,
        limit: 40,
      }),
    enabled: conversationId !== null,
    staleTime: 30_000,
  });

  const { nodes, edges, truncated, readingNearIds } = useMemo(() => {
    const nodeMap = new Map<string, GraphNode>();
    const edgeList: GraphEdge[] = [];
    const addNode = (node: GraphNode) => {
      if (!nodeMap.has(node.id)) nodeMap.set(node.id, node);
    };
    const addEdge = (from: string, to: string, kind: EdgeKind) => {
      if (from === to) return;
      if (!nodeMap.has(from) || !nodeMap.has(to)) return;
      if (edgeList.some((edge) => edge.from === from && edge.to === to && edge.kind === kind)) {
        return;
      }
      edgeList.push({ from, to, kind });
    };
    const docNodeId = `doc:${documentId}`;
    addNode({
      id: docNodeId,
      kind: "document",
      label: detail.data?.displayName ?? "当前文档",
      detail: detail.data?.format?.toUpperCase(),
    });

    const sectionIds = new Map<string, string>();
    const sectionIdFor = (name: string): string => {
      const existing = sectionIds.get(name);
      if (existing !== undefined) return existing;
      const id = `section:${name}`;
      sectionIds.set(id, id);
      addNode({ id, kind: "section", label: name, detail: "章节" });
      addEdge(docNodeId, id, "contains");
      return id;
    };

    for (const item of items) {
      if (item.annotation !== null) {
        const nodeId = `annotation:${item.annotation.id}`;
        addNode({
          id: nodeId,
          kind: "annotation",
          label: item.title,
          detail: `${NODE_LABELS.annotation} · ${item.location ?? "全文"}`,
          excerpt: item.summary || item.quote || "",
          annotationId: item.annotation.id,
          locator: item.locator,
        });
        addEdge(docNodeId, nodeId, "contains");
        const section = sectionName(item.locator);
        if (section !== null) addEdge(nodeId, sectionIdFor(section), "located");
      } else if (item.note !== null) {
        const nodeId = `note:${item.note.id}`;
        addNode({
          id: nodeId,
          kind: "note",
          label: item.title,
          detail: `笔记 · ${item.location ?? "文档级"}`,
          excerpt: item.summary,
          noteId: item.note.id,
          locator: item.locator,
        });
        addEdge(docNodeId, nodeId, "contains");
        const section = sectionName(item.locator);
        if (section !== null) addEdge(nodeId, sectionIdFor(section), "located");
        for (const ref of item.note.sourceRefs) {
          if (ref.sourceType === "annotation") {
            const annotationNode = `annotation:${ref.sourceId}`;
            if (nodeMap.has(annotationNode)) addEdge(annotationNode, nodeId, "related");
          }
        }
      }
    }

    // 标注与同 anchor 的笔记:真实关联(annotation.anchorId === note.anchorId)。
    for (const item of items) {
      if (item.annotation === null) continue;
      const note = items.find(
        (candidate) =>
          candidate.note !== null && candidate.note.anchorId === item.annotation!.anchorId,
      );
      if (note?.note != null) {
        addEdge(`annotation:${item.annotation.id}`, `note:${note.note.id}`, "related");
      }
    }

    for (const bookmark of bookmarks) {
      const id = `bookmark:${bookmark.id}`;
      addNode({
        id,
        kind: "bookmark",
        label: bookmark.label || "书签",
        detail: `书签 · ${bookmark.pageNumber != null ? `第 ${bookmark.pageNumber} 页` : "全文"}`,
        excerpt: bookmark.note,
        bookmark,
        locator: bookmark.locator,
      });
      addEdge(docNodeId, id, "contains");
      const section = sectionName(bookmark.locator);
      if (section !== null) addEdge(id, sectionIdFor(section), "located");
    }

    // 问答引用:只取当前会话真实回答里的 citations;引用指向所属文档。
    for (const message of messages.data?.items ?? []) {
      const content = message.content as MessageContent;
      if (content.schemaVersion !== "answer.v1") continue;
      for (const citation of content.content.citations) {
        const id = `citation:${citation.id}`;
        addNode({
          id,
          kind: "citation",
          label: citation.quotedText.slice(0, 40),
          detail: `问答引用 · 块 ${citation.blockId}`,
          excerpt: citation.quotedText,
          citationId: citation.id,
        });
        addEdge(id, docNodeId, "citation");
      }
    }

    let truncatedCount = 0;
    if (nodeMap.size > MAX_NODES) {
      truncatedCount = nodeMap.size - MAX_NODES;
      const keep = new Set<string>();
      const priorities: NodeKind[] = [
        "document",
        "section",
        "note",
        "annotation",
        "bookmark",
        "citation",
      ];
      for (const kind of priorities) {
        for (const node of nodeMap.values()) {
          if (node.kind === kind && keep.size < MAX_NODES) keep.add(node.id);
        }
      }
      for (const id of [...nodeMap.keys()]) {
        if (!keep.has(id)) nodeMap.delete(id);
      }
      for (let index = edgeList.length - 1; index >= 0; index -= 1) {
        const edge = edgeList[index]!;
        if (!nodeMap.has(edge.from) || !nodeMap.has(edge.to)) edgeList.splice(index, 1);
      }
    }

    // 阅读邻域:PDF 用当前页 ±2;Markdown/Word 用当前标题路径。
    const nearIds = new Set<string>();
    if (viewport !== undefined) {
      const page = viewport.pageNumber ?? null;
      const headingPath = viewport.headingPath ?? [];
      const heading = headingPath[headingPath.length - 1] ?? null;
      for (const node of nodeMap.values()) {
        if (node.kind === "document") continue;
        if (page !== null) {
          const nodePage = locatorPage(node.locator);
          if (nodePage !== null && Math.abs(nodePage - page) <= 2) nearIds.add(node.id);
        }
        if (heading !== null) {
          const path = headingPathOf(node.locator);
          if (path.includes(heading) || (node.kind === "section" && node.label === heading)) {
            nearIds.add(node.id);
          }
        }
      }
    }

    // 聚焦:1-2 跳邻域(沿无向边 BFS)。
    let visibleMap = nodeMap;
    if (focusId !== null && nodeMap.has(focusId)) {
      const adjacency = new Map<string, Set<string>>();
      for (const edge of edgeList) {
        if (!adjacency.has(edge.from)) adjacency.set(edge.from, new Set());
        if (!adjacency.has(edge.to)) adjacency.set(edge.to, new Set());
        adjacency.get(edge.from)!.add(edge.to);
        adjacency.get(edge.to)!.add(edge.from);
      }
      const keep = new Set<string>([focusId]);
      let frontier = new Set<string>([focusId]);
      for (let hop = 0; hop < 2; hop += 1) {
        const next = new Set<string>();
        for (const id of frontier) {
          for (const neighbor of adjacency.get(id) ?? []) {
            if (!keep.has(neighbor)) {
              keep.add(neighbor);
              next.add(neighbor);
            }
          }
        }
        frontier = next;
        if (frontier.size === 0) break;
      }
      visibleMap = new Map([...nodeMap].filter(([id]) => keep.has(id)));
    }

    const list = [...visibleMap.values()].filter((node) => !hidden.has(node.kind));
    const listIds = new Set(list.map((node) => node.id));
    const visibleEdges = edgeList.filter(
      (edge) => listIds.has(edge.from) && listIds.has(edge.to),
    );
    if (layoutMode === "force") layoutForce(list, visibleEdges);
    else layoutHierarchy(list);

    return {
      nodes: list,
      edges: visibleEdges,
      truncated: truncatedCount,
      readingNearIds: new Set([...nearIds].filter((id) => listIds.has(id))),
    };
  }, [bookmarks, detail.data, documentId, focusId, hidden, items, layoutMode, messages.data, viewport]);

  const shownNodes = useMemo(
    () =>
      readingNearOnly && readingNearIds.size > 0
        ? nodes.filter((node) => node.kind === "document" || readingNearIds.has(node.id))
        : nodes,
    [nodes, readingNearIds, readingNearOnly],
  );
  const shownIds = useMemo(() => new Set(shownNodes.map((node) => node.id)), [shownNodes]);
  const shownEdges = useMemo(
    () => edges.filter((edge) => shownIds.has(edge.from) && shownIds.has(edge.to)),
    [edges, shownIds],
  );

  const normalizedQuery = query.trim().toLowerCase();
  const matchedIds = useMemo(() => {
    if (normalizedQuery === "") return null;
    return new Set(
      shownNodes
        .filter((node) =>
          `${node.label} ${node.detail ?? ""} ${node.excerpt ?? ""}`
            .toLowerCase()
            .includes(normalizedQuery),
        )
        .map((node) => node.id),
    );
  }, [shownNodes, normalizedQuery]);

  const nodeById = useMemo(() => new Map(shownNodes.map((node) => [node.id, node])), [shownNodes]);
  const active = activeId === null ? null : (nodeById.get(activeId) ?? null);
  const connectedCount = useMemo(() => {
    if (active === null) return 0;
    return shownEdges.filter((edge) => edge.from === active.id || edge.to === active.id).length;
  }, [active, shownEdges]);

  const closeFullscreen = (): void => {
    setFullscreen(false);
  };

  useEffect(() => {
    if (!fullscreen) return;
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key === "Escape") setFullscreen(false);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [fullscreen]);

  function jumpToNode(node: GraphNode): void {
    if (node.kind === "annotation" && node.annotationId !== undefined) {
      useReaderTools.getState().requestNotesFocus(node.annotationId);
      onToast("success", "已在笔记面板聚焦该标注");
      return;
    }
    if (node.kind === "note" && node.noteId !== undefined) {
      useReaderTools.getState().requestNoteFocus(node.noteId);
      onToast("success", "已打开关联笔记");
      return;
    }
    if (node.kind === "bookmark" && node.bookmark !== undefined) {
      jumpToLocator(documentId, node.bookmark.locator);
      return;
    }
    if (node.kind === "section") {
      const locator: SourceLocator = {
        kind: "markdown",
        startOffset: 0,
        endOffset: 1,
        mdastNodeType: "heading",
        headingPath: [node.label],
        locatorVersion: 1,
      };
      jumpToLocator(documentId, locator);
      return;
    }
    if (node.kind === "citation" && node.citationId !== undefined) {
      void resolveCitationAndGoto(client, documentId, node.citationId).then((ok) => {
        if (!ok) onToast("error", "引用位置已失效,未能跳转");
      });
      return;
    }
    if (node.locator != null) jumpToLocator(documentId, node.locator);
  }

  function selectNode(node: GraphNode): void {
    setActiveId(node.id);
  }

  function fitView(): void {
    if (shownNodes.length === 0) return;
    const xs = shownNodes.map((node) => node.x ?? 0);
    const ys = shownNodes.map((node) => node.y ?? 0);
    const minX = Math.min(...xs) - 60;
    const maxX = Math.max(...xs) + 60;
    const minY = Math.min(...ys) - 50;
    const maxY = Math.max(...ys) + 50;
    const width = Math.max(80, maxX - minX);
    const height = Math.max(80, maxY - minY);
    const nextZoom = clamp(Math.min(VIEW_W / width, VIEW_H / height), MIN_ZOOM, 1.5);
    setZoom(nextZoom);
    setPan({
      x: VIEW_W / 2 - ((minX + maxX) / 2) * nextZoom,
      y: VIEW_H / 2 - ((minY + maxY) / 2) * nextZoom,
    });
  }

  function resetView(): void {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  }

  function zoomBy(factor: number): void {
    setZoom((current) => clamp(current * factor, MIN_ZOOM, MAX_ZOOM));
  }

  function onWheel(event: ReactWheelEvent<SVGSVGElement>): void {
    event.preventDefault();
    const svg = svgRef.current;
    if (svg === null) return;
    const factor = event.deltaY < 0 ? 1.12 : 1 / 1.12;
    const nextZoom = clamp(zoom * factor, MIN_ZOOM, MAX_ZOOM);
    if (nextZoom === zoom) return;
    const matrix = svg.getScreenCTM();
    if (matrix === null) {
      setZoom(nextZoom);
      return;
    }
    const point = svg.createSVGPoint();
    point.x = event.clientX;
    point.y = event.clientY;
    const svgPoint = point.matrixTransform(matrix.inverse());
    const worldX = (svgPoint.x - pan.x) / zoom;
    const worldY = (svgPoint.y - pan.y) / zoom;
    setZoom(nextZoom);
    setPan({ x: svgPoint.x - worldX * nextZoom, y: svgPoint.y - worldY * nextZoom });
  }

  function onCanvasPointerDown(event: ReactPointerEvent<SVGSVGElement>): void {
    if (event.button !== 0) return;
    const target = event.target as Element;
    if (target.closest(".kg-node") !== null) return;
    const originX = event.clientX;
    const originY = event.clientY;
    const startPan = pan;
    let moved = false;
    const onMove = (ev: PointerEvent): void => {
      const dx = ev.clientX - originX;
      const dy = ev.clientY - originY;
      if (Math.abs(dx) + Math.abs(dy) > 3) moved = true;
      setPan({ x: startPan.x + dx, y: startPan.y + dy });
    };
    const onEnd = (): void => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onEnd);
      window.removeEventListener("pointercancel", onEnd);
      if (!moved) setActiveId(null);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onEnd);
    window.addEventListener("pointercancel", onEnd);
  }

  const nodeShape = (node: GraphNode, radius: number): ReactNode => {
    switch (node.kind) {
      case "document":
        return <rect x={-36} y={-19} width={72} height={38} rx={9} />;
      case "section":
        return (
          <polygon
            points={`0,${-radius} ${radius * 0.87},${-radius / 2} ${radius * 0.87},${radius / 2} 0,${radius} ${-radius * 0.87},${radius / 2} ${-radius * 0.87},${-radius / 2}`}
          />
        );
      case "note":
        return <rect x={-radius * 0.85} y={-radius * 0.85} width={radius * 1.7} height={radius * 1.7} rx={3} />;
      case "bookmark":
        return (
          <polygon
            points={`0,${-radius} ${radius},0 0,${radius} ${-radius},0`}
          />
        );
      case "citation":
        return <polygon points={`0,${-radius} ${radius},${radius * 0.8} ${-radius},${radius * 0.8}`} />;
      default:
        return <circle r={radius} />;
    }
  };

  const focusNode = focusId === null ? null : (nodeById.get(focusId) ?? null);
  const empty = shownNodes.filter((node) => node.kind !== "document").length === 0;

  return (
    <div
      className={`knowledge-graph${fullscreen ? " kg-fullscreen" : ""}`}
      aria-label="知识图"
      role={fullscreen ? "dialog" : undefined}
      aria-modal={fullscreen ? "true" : undefined}
    >
      <div className="kg-toolbar" role="group" aria-label="知识图过滤">
        <span className="kg-search">
          <Search className="w-3 h-3" aria-hidden="true" />
          <input
            type="search"
            value={query}
            placeholder="搜索节点/摘录"
            aria-label="搜索知识图节点"
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key !== "Enter" || matchedIds === null) return;
              const first = shownNodes.find((node) => matchedIds.has(node.id));
              if (first !== undefined) {
                setActiveId(first.id);
                event.currentTarget.blur();
              }
            }}
          />
          {normalizedQuery !== "" ? (
            <span className="kg-search-count" role="status">
              {matchedIds?.size ?? 0}/{shownNodes.length}
            </span>
          ) : null}
        </span>
        {(Object.keys(NODE_LABELS) as NodeKind[]).map((kind) => (
          <label key={kind} className={`kg-filter kind-${kind}`}>
            <input
              type="checkbox"
              checked={!hidden.has(kind)}
              onChange={(event) =>
                setHidden((prev) => {
                  const next = new Set(prev);
                  if (event.target.checked) next.delete(kind);
                  else next.add(kind);
                  return next;
                })
              }
            />
            <span className="kg-filter-dot" aria-hidden="true" />
            {NODE_LABELS[kind]}
          </label>
        ))}
        <span className="kg-count">
          {shownNodes.length} 节点 · {shownEdges.length} 边
        </span>
      </div>

      <div className="kg-controls" role="group" aria-label="知识图视图控制">
        <div className="kg-segmented" role="group" aria-label="布局">
          <button
            type="button"
            className={layoutMode === "hierarchy" ? "active" : ""}
            aria-pressed={layoutMode === "hierarchy"}
            onClick={() => setLayoutMode("hierarchy")}
          >
            层级
          </button>
          <button
            type="button"
            className={layoutMode === "force" ? "active" : ""}
            aria-pressed={layoutMode === "force"}
            onClick={() => setLayoutMode("force")}
          >
            力导向
          </button>
        </div>
        <button type="button" className="kg-icon-btn" aria-label="缩小知识图" title="缩小" onClick={() => zoomBy(1 / 1.15)}>
          <ZoomOut className="w-3.5 h-3.5" aria-hidden="true" />
        </button>
        <button type="button" className="kg-icon-btn" aria-label="放大知识图" title="放大" onClick={() => zoomBy(1.15)}>
          <ZoomIn className="w-3.5 h-3.5" aria-hidden="true" />
        </button>
        <button type="button" className="kg-icon-btn" aria-label="适配视图" title="适配视图" onClick={fitView}>
          <Crosshair className="w-3.5 h-3.5" aria-hidden="true" />
        </button>
        <button type="button" className="kg-icon-btn" aria-label="重置缩放" title="重置缩放" onClick={resetView}>
          <RotateCcw className="w-3.5 h-3.5" aria-hidden="true" />
        </button>
        <button
          type="button"
          className={`kg-chip${readingNearOnly ? " active" : ""}`}
          aria-pressed={readingNearOnly}
          aria-label="只看当前阅读附近"
          disabled={readingNearIds.size === 0}
          title={
            readingNearIds.size === 0
              ? "当前阅读位置附近没有关联节点"
              : `只看当前阅读附近(${readingNearIds.size} 个节点)`
          }
          onClick={() => setReadingNearOnly((value) => !value)}
        >
          <Focus className="w-3 h-3" aria-hidden="true" />
          阅读邻域{readingNearIds.size > 0 ? ` ${readingNearIds.size}` : ""}
        </button>
        {focusNode !== null ? (
          <button
            type="button"
            className="kg-chip active"
            aria-label="显示全部节点"
            onClick={() => setFocusId(null)}
          >
            聚焦:{focusNode.label.slice(0, 12)} · 显示全部
          </button>
        ) : null}
        <button
          type="button"
          className="kg-icon-btn kg-fullscreen-btn"
          aria-label={fullscreen ? "退出知识图全屏" : "知识图全屏"}
          title={fullscreen ? "退出全屏" : "全屏"}
          onClick={() => setFullscreen((value) => !value)}
        >
          {fullscreen ? (
            <Minimize2 className="w-3.5 h-3.5" aria-hidden="true" />
          ) : (
            <Maximize2 className="w-3.5 h-3.5" aria-hidden="true" />
          )}
        </button>
      </div>

      {truncated > 0 ? (
        <p className="kg-truncated" role="status">
          节点超过上限 {MAX_NODES} 个,已截断 {truncated} 个;可关闭部分类型过滤后查看。
        </p>
      ) : null}
      <div className="kg-legend" aria-label="图例">
        <span>边:</span>
        {(Object.keys(EDGE_LABELS) as EdgeKind[]).map((kind) => (
          <span key={kind} className={`kg-legend-edge kind-${kind}`}>
            {EDGE_LABELS[kind]}
          </span>
        ))}
        <span className="kg-legend-note">支持/冲突:无可靠数据时不生成(不推断)</span>
        {readingNearIds.size > 0 ? <span className="kg-legend-near">◌ 当前阅读附近</span> : null}
      </div>

      {detail.isError ? (
        <p className="kg-error" role="alert">
          文档信息加载失败,知识图可能不完整;请重试或稍后再打开。
        </p>
      ) : null}

      <div className="kg-main">
        <svg
          ref={svgRef}
          className="kg-canvas"
          viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
          role="img"
          aria-label={`知识图关系:${shownNodes.length} 个节点,${shownEdges.length} 条边`}
          onWheel={onWheel}
          onPointerDown={onCanvasPointerDown}
        >
          <defs>
            <marker id="kg-arrow" markerWidth="8" markerHeight="8" refX="7" refY="3" orient="auto">
              <path d="M0,0 L0,6 L7,3 z" fill="var(--line-strong, #c7cbd4)" />
            </marker>
          </defs>
          <g transform={`translate(${pan.x},${pan.y}) scale(${zoom})`}>
            {shownEdges.map((edge, index) => {
              const from = nodeById.get(edge.from);
              const to = nodeById.get(edge.to);
              if (from === undefined || to === undefined) return null;
              const dimmed =
                matchedIds !== null && !(matchedIds.has(edge.from) && matchedIds.has(edge.to));
              return (
                <line
                  key={`${edge.from}-${edge.to}-${edge.kind}-${index}`}
                  x1={from.x}
                  y1={from.y}
                  x2={to.x}
                  y2={to.y}
                  className={`kg-edge kind-${edge.kind}${dimmed ? " dimmed" : ""}`}
                  markerEnd="url(#kg-arrow)"
                />
              );
            })}
            {shownNodes.map((node) => {
              const radius = node.kind === "document" ? 22 : node.kind === "section" ? 17 : 12;
              const dimmed = matchedIds !== null && !matchedIds.has(node.id);
              const classes = [
                "kg-node",
                `kind-${node.kind}`,
                activeId === node.id ? "active" : "",
                readingNearIds.has(node.id) ? "reading-near" : "",
                dimmed ? "dimmed" : "",
              ]
                .filter((value) => value !== "")
                .join(" ");
              return (
                <g
                  key={node.id}
                  className={classes}
                  transform={`translate(${node.x ?? 0},${node.y ?? 0})`}
                  role="button"
                  tabIndex={0}
                  aria-label={`${NODE_LABELS[node.kind]}:${node.label}`}
                  onClick={(event) => {
                    event.stopPropagation();
                    selectNode(node);
                  }}
                  onDoubleClick={(event) => {
                    event.stopPropagation();
                    setActiveId(node.id);
                    setFocusId(node.id);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      selectNode(node);
                    }
                  }}
                >
                  {/* 透明命中区:放大可点击范围,也让 bbox 中心落在节点中心。 */}
                  <circle r={radius + 9} className="kg-node-hit" />
                  {nodeShape(node, radius)}
                  <text
                    textAnchor="middle"
                    dy={node.kind === "document" ? 35 : node.kind === "section" ? 31 : 26}
                  >
                    {node.label.slice(0, 18)}
                  </text>
                </g>
              );
            })}
          </g>
        </svg>

        <aside className="kg-detail" role="status" aria-label="节点详情">
          {active === null ? (
            <p>点击节点查看来源与摘录;双击聚焦邻居;拖动画布平移,滚轮缩放。</p>
          ) : (
            <>
              <strong>
                {NODE_LABELS[active.kind]} · {active.label}
              </strong>
              {active.detail !== undefined ? <span>{active.detail}</span> : null}
              {active.excerpt !== undefined && active.excerpt.trim() !== "" ? (
                <span className="kg-detail-excerpt">“{clip(active.excerpt, 110)}”</span>
              ) : null}
              <span className="kg-detail-meta">关联 {connectedCount} 条边</span>
              <div className="kg-detail-actions">
                <button
                  type="button"
                  className="btn tiny"
                  aria-label="回到原文"
                  onClick={() => jumpToNode(active)}
                  disabled={
                    active.kind === "document" ||
                    (active.annotationId === undefined &&
                      active.noteId === undefined &&
                      active.bookmark === undefined &&
                      active.citationId === undefined &&
                      active.locator == null)
                  }
                >
                  回到原文
                </button>
                <button
                  type="button"
                  className="btn tiny ghost"
                  aria-label="聚焦该节点的邻居"
                  onClick={() => setFocusId(active.id)}
                >
                  聚焦邻居
                </button>
                {focusId !== null ? (
                  <button
                    type="button"
                    className="btn tiny ghost"
                    aria-label="显示全部节点"
                    onClick={() => setFocusId(null)}
                  >
                    显示全部
                  </button>
                ) : null}
              </div>
            </>
          )}
        </aside>
      </div>

      {empty ? (
        <p className="kg-empty">
          本篇还没有标注、笔记、书签或问答引用;在原文标注或提问后,这里会显示真实关联。
        </p>
      ) : null}
      {fullscreen ? (
        <button type="button" className="kg-fullscreen-close" aria-label="退出知识图全屏" onClick={closeFullscreen}>
          <Minimize2 className="w-4 h-4" aria-hidden="true" />
        </button>
      ) : null}
    </div>
  );
}
