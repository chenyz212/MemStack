<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import {
  apiFetch,
  ApiRequestError,
  type CursorPage,
  type ConclusionCardPayload,
  type DeletedProjectStats,
  type EmbeddingSettings,
  type GraphResult,
  type MemoryItem,
  type MemoryFacets,
  type MemoryCandidateItem,
  type McpConnectionTestResult,
  type McpConnectionInfo,
  type McpClientCard,
  type McpClientSecret,
  type McpPermission,
  type McpTokenItem,
  type McpTokenSecret,
  type OverviewResult,
  type PathHealthReport,
  type ProjectItem,
  type RegistrationReport,
  type SearchResult,
} from "./api";
import {
  getLogoColor,
  getLogoLetter,
  getDisplayTitle,
  getCatalogEntry,
  isFallbackEntry,
} from "./mcpClientCatalog";
import { renderMemoryMarkdown } from "./markdown";
import GraphPage from "./graph/GraphPage.vue";
import { renderGraphPreview } from "./graph/preview";
import ProjectDocumentsPage from "./project-documents/ProjectDocumentsPage.vue";
import ConclusionCardFields from "./project-documents/ConclusionCardFields.vue";
import PromptDialog from "./ai-prompt/PromptDialog.vue";

type NavigationKey =
  | "overview"
  | "memories"
  | "documents"
  | "graph"
  | "connections"
  | "settings";
type MemoryViewKey =
  | "ALL"
  | "PERSONAL"
  | "PROJECT_ALL"
  | "FAVORITE"
  | "PINNED"
  | "ARCHIVED"
  | "PENDING"
  | "PROJECT";

interface NavigationItem {
  key: NavigationKey;
  label: string;
  icon: string;
}

interface MemoryFormState {
  id: string | null;
  scope: "Personal" | "Project";
  projectId: string;
  title: string;
  summary: string;
  content: string;
  memoryType: string;
  keywords: string;
  tags: string;
  importance: number;
  cloudProcessingAllowed: boolean;
  expectedVersion: number | null;
}

interface McpClientDraft {
  displayName: string;
  permission: McpPermission;
  projectId: string;
  validityDays: number;
}

/** 图谱功能暂时隐藏，置为 true 可恢复全部入口。 */
const GRAPH_FEATURE_ENABLED = false;

const navigationItems: NavigationItem[] = [
  { key: "overview", label: "总览", icon: "⌂" },
  { key: "memories", label: "记忆", icon: "✦" },
  { key: "documents", label: "项目文档", icon: "❑" },
  { key: "graph", label: "图谱", icon: "◌" },
  { key: "connections", label: "连接 AI", icon: "⌁" },
  { key: "settings", label: "设置", icon: "⚙" },
];

const visibleNavigationItems = navigationItems.filter(
  (item) => GRAPH_FEATURE_ENABLED || item.key !== "graph",
);

const memoryTypeLabels: Record<string, string> = {
  NOTE: "笔记",
  PREFERENCE: "偏好",
  DECISION: "决策",
  SOLUTION: "方案",
  FACT: "事实",
  CONVENTION: "约定",
  TASK: "任务",
  CONTEXT: "上下文",
  OTHER: "其他",
};

/** 将内部记忆类型转换为用户可读的中文名称。 */
function getMemoryTypeLabel(memoryType: string): string {
  return memoryTypeLabels[memoryType.toUpperCase()] ?? "记忆";
}

/** 返回记忆卡片对应的稳定视觉类型类名。 */
function getMemoryTypeClass(memoryType: string): string {
  const normalized = memoryType.toUpperCase();
  return memoryTypeLabels[normalized]
    ? `memory-type-${normalized.toLowerCase()}`
    : "memory-type-other";
}

const activeNavigation = ref<NavigationKey>("overview");
const isDarkTheme = ref(window.localStorage.getItem("theme") === "dark");
const overview = ref<OverviewResult | null>(null);
const projects = ref<ProjectItem[]>([]);
/** 从记忆详情进入图谱的入口记忆（两跳局部图谱中心）。 */
const graphEntryMemoryId = ref<string | null>(null);
/** 总览图谱预览数据（24 节点真实关系）。 */
const overviewGraph = ref<GraphResult | null>(null);
const overviewGraphCanvas = ref<HTMLCanvasElement | null>(null);
/** 窗口隐藏标志：暂停总览轨道环绕动画。 */
const isWindowHidden = ref(false);
const activeProjects = computed(() =>
  projects.value.filter((project) => !project.isArchived),
);
const archivedProjects = computed(() =>
  projects.value.filter((project) => project.isArchived),
);
const memories = ref<MemoryItem[]>([]);
const candidates = ref<MemoryCandidateItem[]>([]);
const candidateScopeFilter = ref("");
const candidateSourceFilter = ref("");
const editingCandidateId = ref<string | null>(null);
/** 当前候选是否携带结论卡片结构化载荷。 */
const editingConclusionCandidate = ref(false);
/** 打开候选编辑器时的结构化载荷探测状态。 */
const isLoadingCandidateStructure = ref(false);
const memoryFacets = ref<MemoryFacets>({
  allCount: 0,
  personalCount: 0,
  projectCount: 0,
  favoriteCount: 0,
  pinnedCount: 0,
  archivedCount: 0,
  projectCounts: {},
});
const nextCursor = ref<string | null>(null);
const quickCapture = ref("");
const searchQuery = ref("");
const searchResults = ref<SearchResult[]>([]);
/** 搜索已排队或请求进行中：空列表时显示"正在搜索"而非"未找到"。 */
const isSearching = ref(false);
const selectedMemory = ref<MemoryItem | null>(null);
const selectedProjectId = ref("");
const memoryView = ref<MemoryViewKey>("ALL");
const listType = ref("");
const listTag = ref("");
const listImportance = ref(1);
const isLoading = ref(false);
const isSaving = ref(false);
const errorMessage = ref("");
const toastMessage = ref("");
/** 错误提示 3 秒后自动消失 */
watch(errorMessage, (next) => {
  if (next.length === 0) return;
  window.setTimeout(() => {
    if (errorMessage.value === next) errorMessage.value = "";
  }, 3000);
});
const showMemoryEditor = ref(false);
const showProjectEditor = ref(false);
const archiveConfirmationMemory = ref<MemoryItem | null>(null);
const deleteConfirmationMemory = ref<MemoryItem | null>(null);
const isConfirmingMemoryAction = ref(false);
const projectName = ref("");
const projectDescription = ref("");
const projectColor = ref("#4f8cff");
const projectWorkspaceIdentifier = ref("");
const editingProjectId = ref<string | null>(null);
const embedding = ref<EmbeddingSettings>({
  baseUrl: "https://api.openai.com/v1",
  model: "text-embedding-3-small",
  apiKey: "",
  dimensions: 1024,
  enabled: false,
  configured: false,
});
/** 编辑中的 API Key：进入编辑态时预填当前明文，修改后保存。 */
const embeddingApiKey = ref("");
/** API Key 是否处于编辑态：已配置时默认雾化展示（悬停显形），点击进入编辑。 */
const isEmbeddingKeyEditing = ref(false);

/** 点击已配置的 API Key 进入编辑：预填当前明文，不清空。 */
function editEmbeddingKey(): void {
  embeddingApiKey.value = embedding.value.apiKey;
  isEmbeddingKeyEditing.value = true;
}

/** 取消编辑：回到雾化展示态，不改动现有配置。 */
function cancelEmbeddingKeyEdit(): void {
  embeddingApiKey.value = "";
  isEmbeddingKeyEditing.value = false;
}

/** 单击/双击区分：单击复制 API Key，双击进入编辑。 */
let apiKeyClickTimer: number | null = null;

/** 单击：延迟 240ms 执行复制（若期间发生双击则取消，避免误复制）。 */
function onApiKeyClick(): void {
  if (apiKeyClickTimer !== null) return;
  apiKeyClickTimer = window.setTimeout(() => {
    apiKeyClickTimer = null;
    void copyMcpValue(embedding.value.apiKey, "API Key 已复制");
  }, 240);
}

/** 双击：取消挂起的复制并进入编辑。 */
function onApiKeyDblclick(): void {
  if (apiKeyClickTimer !== null) {
    window.clearTimeout(apiKeyClickTimer);
    apiKeyClickTimer = null;
  }
  editEmbeddingKey();
}
const mcpConnection = ref<McpConnectionInfo | null>(null);
const mcpClients = ref<McpClientCard[]>([]);
/** 是否展示统一连接弹窗 */
const showMcpClientDialog = ref(false);
/** 弹窗当前活动标签：issue=编辑 AI 工具 / config=接入配置 */
const mcpDialogTab = ref<"issue" | "config">("issue");
/** 弹窗打开时关联的客户端会话主键；新增卡片时为 null */
const editingMcpClient = ref<McpClientCard | null>(null);
/** 弹窗表单草稿 */
const mcpClientDraft = ref<McpClientDraft>({
  displayName: "",
  permission: "ReadWrite",
  projectId: "",
  validityDays: 0,
});
/** 本次会话签发/轮换产生的明文令牌，关闭弹窗或切页后清除 */
const issuedMcpClientToken = ref<string | null>(null);
/** 接入配置视图选中的客户端会话主键 */
const selectedMcpClientSessionId = ref<string | null>(null);
/** 后端生成的 stdio 接入配置报告（预览或注册结果） */
const mcpConfigReport = ref<RegistrationReport | null>(null);
/** 接入配置预览加载中标志 */
const isMcpConfigLoading = ref(false);
/** 「写入配置」进行中标志，避免重复提交 */
const isMcpRegistering = ref(false);
/** 弹窗操作进行中标志，避免重复提交 */
const isMcpClientSaving = ref(false);
/** 客户端配置路径健康（键=会话 ID；绿色版移动目录后提示失效与一键修复） */
const mcpPathHealth = ref<Record<string, PathHealthReport>>({});
/** 路径健康刷新进行中标志 */
const isMcpPathHealthLoading = ref(false);
/** 全局提示词查看弹窗可见标志 */
const promptDialogVisible = ref(false);
/** 弹窗对应的客户端类型（适配器输入：Codex/Claude/Cursor/Generic） */
const promptDialogClientType = ref("");
/** 弹窗对应的客户端显示名 */
const promptDialogClientName = ref("");
let searchTimer: number | null = null;
let searchController: AbortController | null = null;

const memoryForm = ref<MemoryFormState>(createEmptyMemoryForm());
const pageTitle = computed(
  () =>
    navigationItems.find((item) => item.key === activeNavigation.value)
      ?.label ?? "总览",
);
const memoryViewTitle = computed(() => {
  if (memoryView.value === "PERSONAL") return "个人记忆";
  if (memoryView.value === "PROJECT_ALL") return "项目记忆";
  if (memoryView.value === "FAVORITE") return "收藏记忆";
  if (memoryView.value === "PINNED") return "置顶记忆";
  if (memoryView.value === "ARCHIVED") return "已归档";
  if (memoryView.value === "PENDING") return "待确认";
  if (memoryView.value === "PROJECT")
    return (
      activeProjects.value.find(
        (project) => project.id === selectedProjectId.value,
      )?.name ?? "项目记忆"
    );
  return "全部记忆";
});
const memoryViewDescription = computed(() => {
  if (memoryView.value === "PERSONAL")
    return "跨项目通用的偏好、习惯与长期经验";
  if (memoryView.value === "PROJECT_ALL") return "来自所有工作空间的项目上下文";
  if (memoryView.value === "FAVORITE") return "你主动收藏的重要内容";
  if (memoryView.value === "PINNED") return "始终优先展示的关键记忆";
  if (memoryView.value === "ARCHIVED") return "不再活跃但仍可恢复的内容";
  if (memoryView.value === "PENDING")
    return "AI 提交但尚未进入正式检索的候选记忆";
  if (memoryView.value === "PROJECT") return "仅显示当前中文项目中的记忆";
  return "个人记忆与项目记忆的统一视图";
});
/** 搜索态下的空列表文案：区分"正在搜索 / 未找到"，避免误引导创建记忆。 */
const isSearchActive = computed(() => searchQuery.value.trim().length > 0);
/** 当前列表是否展示后端搜索结果（搜索激活且非归档视图，归档走本地过滤）。 */
const isSearchResultView = computed(
  () => isSearchActive.value && memoryView.value !== "ARCHIVED",
);
/** 搜索结果语义相似度映射（memoryId → 余弦值），供卡片匹配度展示。 */
const searchSimilarityById = computed(() => {
  const map = new Map<string, number>();
  for (const result of searchResults.value) {
    if (result.semanticSimilarity != null)
      map.set(result.memory.id, result.semanticSimilarity);
  }
  return map;
});

/** 搜索结果卡片的匹配度文案：语义相似度百分比，纯关键词命中显示"关键词"。 */
function matchLabel(memoryId: string): string {
  const similarity = searchSimilarityById.value.get(memoryId);
  if (similarity === undefined) return "关键词";
  return `匹配 ${Math.round(similarity * 100)}%`;
}
const memoryEmptyTitle = computed(() => {
  if (isSearchActive.value && isSearching.value) return "正在搜索…";
  if (isSearchActive.value) return "未找到相关记忆";
  if (memoryView.value === "ARCHIVED") return "没有已归档记忆";
  return `${memoryViewTitle.value}还是空的`;
});
const memoryEmptyHint = computed(() => {
  if (isSearchActive.value && isSearching.value) return "正在检索本地与语义索引。";
  if (isSearchActive.value)
    return "换个关键词试试，或切换到「全部记忆」扩大搜索范围。";
  return "创建一条清晰、可复用的长期记忆。";
});
const filteredMemories = computed(() => {
  const query = searchQuery.value.trim().toLocaleLowerCase();
  if (query && memoryView.value !== "ARCHIVED") {
    return searchResults.value
      .map((result) => result.memory)
      .filter((memory) => {
        if (memoryView.value === "FAVORITE") return memory.isFavorite;
        if (memoryView.value === "PINNED") return memory.isPinned;
        return true;
      });
  }
  return memories.value.filter(
    (memory) =>
      !query ||
      [memory.title, memory.summary, memory.content].some((value) =>
        value.toLocaleLowerCase().includes(query),
      ),
  );
});
const candidateSources = computed(() =>
  [...new Set(candidates.value.map((candidate) => candidate.sourceName))].sort(),
);
/** 已归档视图活动标签：memories=已归档记忆 / projects=已归档项目。 */
const archivedTab = ref<"memories" | "projects">("memories");
/** 已归档项目卡片展开的项目 id（手风琴，再次点击收起）。 */
const expandedArchivedProjectId = ref<string | null>(null);
/** 项目操作确认弹框（归档/彻底删除），居中显示。 */
const projectConfirmation = ref<{
  kind: "archive" | "delete";
  project: ProjectItem;
  memoryCount: number;
} | null>(null);
const isProjectActionRunning = ref(false);
/** 已归档视图分组：个人记忆一组，项目记忆按项目分组（含项目颜色，归档项目排前）。 */
const archivedGroups = computed(() => {
  const groups: {
    key: string;
    title: string;
    color: string | null;
    projectId: string | null;
    items: MemoryItem[];
  }[] = [{ key: "personal", title: "个人记忆", color: null, projectId: null, items: [] }];
  const index = new Map<string, (typeof groups)[number]>();
  for (const memory of filteredMemories.value) {
    const projectId = memory.projectId ?? "";
    if (!projectId) {
      groups[0].items.push(memory);
      continue;
    }
    let group = index.get(projectId);
    if (!group) {
      const project = projects.value.find((item) => item.id === projectId);
      group = {
        key: projectId,
        title: project?.name ?? memory.projectName ?? "未知项目",
        color: project?.color ?? "#8a93a6",
        projectId,
        items: [],
      };
      index.set(projectId, group);
      groups.push(group);
    }
    group.items.push(memory);
  }
  return groups.filter((group) => group.items.length > 0);
});
const filteredCandidates = computed(() =>
  candidates.value.filter((candidate) => {
    if (candidateScopeFilter.value && candidate.scope !== candidateScopeFilter.value)
      return false;
    if (selectedProjectId.value && candidate.projectId !== selectedProjectId.value)
      return false;
    return !candidateSourceFilter.value || candidate.sourceName === candidateSourceFilter.value;
  }),
);

/** 创建一份明确的空记忆表单。 */
function createEmptyMemoryForm(): MemoryFormState {
  return {
    id: null,
    scope: "Personal",
    projectId: "",
    title: "",
    summary: "",
    content: "",
    memoryType: "NOTE",
    keywords: "",
    tags: "",
    importance: 3,
    cloudProcessingAllowed: true,
    expectedVersion: null,
  };
}

/** 切换页面并加载该页面需要的数据。 */
async function selectNavigation(key: NavigationKey): Promise<void> {
  closeMemoryDetails();
  activeNavigation.value = key;
  errorMessage.value = "";
  if (key === "overview") {
    await loadOverview();
    void loadOverviewGraphPreview();
  }
  if (key === "memories") await loadMemories(false);
  if (key === "connections") await loadMcpConnection();
  if (key === "settings") await loadEmbeddingSettings();
  // 图谱页由 GraphPage 自行加载数据。
}

/** 收回记忆详情抽屉并清除仅属于当前详情的状态。 */
function closeMemoryDetails(): void {
  selectedMemory.value = null;
}

/** 点击普通页面区域时收回抽屉，记忆入口和弹窗交互由自身处理。 */
function closeMemoryDetailsOutside(event: MouseEvent): void {
  const target = event.target instanceof Element ? event.target : null;
  if (
    target?.closest(
      ".memory-list article, .recent-list button, .modal-backdrop",
    )
  )
    return;
  if (selectedMemory.value) closeMemoryDetails();
}

/** 加载当前桌面客户端的本机 MCP 连接信息和动态客户端列表。 */
async function loadMcpConnection(): Promise<void> {
  try {
    mcpConnection.value = await apiFetch<McpConnectionInfo>(
      "/api/mcp/connection",
      { method: "GET" },
      null,
    );
    mcpClients.value = await apiFetch<McpClientCard[]>(
      "/api/mcp/clients",
      { method: "GET" },
      null,
    );
    issuedMcpClientToken.value = null;
    void refreshMcpPathHealth();
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 打开指定客户端的全局提示词查看弹窗（§21.5：查看/复制/差异/安装）。 */
function openPromptDialog(client: McpClientCard): void {
  promptDialogClientType.value = client.displayName;
  promptDialogClientName.value = getDisplayTitle(client.displayName);
  promptDialogVisible.value = true;
}

/** 刷新客户端配置路径健康：同一客户端类型只查询一次，结果按会话 ID 关联卡片。 */
async function refreshMcpPathHealth(): Promise<void> {
  if (mcpClients.value.length === 0 || isMcpPathHealthLoading.value) return;
  isMcpPathHealthLoading.value = true;
  try {
    const seenTypes = new Set<string>();
    const reports = new Map<string, PathHealthReport>();
    for (const client of mcpClients.value) {
      if (seenTypes.has(client.displayName)) continue;
      seenTypes.add(client.displayName);
      const report = await apiFetch<PathHealthReport>(
        `/api/mcp/clients/path-health?clientType=${encodeURIComponent(client.displayName)}`,
        { method: "GET" },
        null,
      );
      reports.set(client.displayName, report);
    }
    const next: Record<string, PathHealthReport> = {};
    for (const client of mcpClients.value) {
      const report = reports.get(client.displayName);
      if (report) next[client.sessionId] = report;
    }
    mcpPathHealth.value = next;
  } catch {
    // 健康检测失败不影响页面主流程（如 MCP exe 未找到时保持无警示态）。
  } finally {
    isMcpPathHealthLoading.value = false;
  }
}

/** 一键重新注册：程序位置变化后，把新 MCP exe 路径写入客户端配置（复用注册链路）。 */
async function reRegisterMcpClient(client: McpClientCard): Promise<void> {
  if (isMcpRegistering.value) return;
  if (!window.confirm(
    `检测到 MemStack 程序位置已变化。\n将把新的 MCP 程序路径重新写入「${getDisplayTitle(client.displayName)}」的配置文件（原配置自动备份），继续吗？`,
  )) {
    return;
  }
  isMcpRegistering.value = true;
  try {
    const result = await apiFetch<RegistrationReport>(
      `/api/mcp/clients/${client.sessionId}/register`,
      { method: "POST", body: JSON.stringify({ clientType: client.displayName }) },
      null,
    );
    showToast(result.message);
    await refreshMcpPathHealth();
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    isMcpRegistering.value = false;
  }
}

/** 根据显式有效天数生成过期时间，零表示长期有效。 */
function createTokenExpiration(validityDays: number): string | null {
  if (validityDays === 0) return null;
  return new Date(Date.now() + validityDays * 24 * 60 * 60 * 1000).toISOString();
}

/** 打开新增 AI 工具弹窗：重置草稿与一次性状态。 */
function openNewMcpClientDialog(): void {
  editingMcpClient.value = null;
  mcpClientDraft.value = {
    displayName: "",
    permission: "ReadWrite",
    projectId: "",
    validityDays: 0,
  };
  issuedMcpClientToken.value = null;
  selectedMcpClientSessionId.value = null;
  mcpConfigReport.value = null;
  mcpDialogTab.value = "issue";
  showMcpClientDialog.value = true;
}

/** 打开编辑现有 AI 工具弹窗：用卡片数据回填草稿。 */
function openEditMcpClientDialog(client: McpClientCard, tab: "issue" | "config"): void {
  editingMcpClient.value = client;
  mcpClientDraft.value = {
    displayName: client.displayName,
    permission: client.permission,
    projectId: client.projectId ?? "",
    validityDays: client.expiresAt ? 0 : 0,
  };
  issuedMcpClientToken.value = null;
  selectedMcpClientSessionId.value = client.sessionId;
  mcpConfigReport.value = null;
  mcpDialogTab.value = tab;
  showMcpClientDialog.value = true;
  if (tab === "config") void loadMcpClientConfigPreview(client);
}

/** 切换弹窗标签；切入接入配置且尚无预览时按需加载（stdio 配置不依赖明文令牌）。 */
function switchMcpDialogTab(tab: "issue" | "config"): void {
  mcpDialogTab.value = tab;
  const client = editingMcpClient.value ?? activeMcpClientForConfig.value;
  if (tab === "config" && client && !mcpConfigReport.value && !isMcpConfigLoading.value) {
    void loadMcpClientConfigPreview(client);
  }
}

/** 关闭弹窗并清除一次性状态；令牌明文由页面失焦/Esc 统一清理。 */
function closeMcpClientDialog(): void {
  showMcpClientDialog.value = false;
  editingMcpClient.value = null;
  issuedMcpClientToken.value = null;
  mcpConfigReport.value = null;
}

/** 创建新 AI 工具：事务中建立会话和首个 Token，明文令牌仅展示一次。 */
async function createMcpClient(): Promise<void> {
  const name = mcpClientDraft.value.displayName.trim();
  if (!name) {
    errorMessage.value = "请填写 AI 工具名";
    return;
  }
  if (isMcpClientSaving.value) return;
  isMcpClientSaving.value = true;
  try {
    const secret = await apiFetch<McpClientSecret>(
      "/api/mcp/clients",
      {
        method: "POST",
        body: JSON.stringify({
          displayName: name,
          permission: mcpClientDraft.value.permission,
          projectId: mcpClientDraft.value.projectId || null,
          expiresAt: createTokenExpiration(mcpClientDraft.value.validityDays),
        }),
      },
      null,
    );
    issuedMcpClientToken.value = secret.plainToken;
    editingMcpClient.value = secret.client;
    selectedMcpClientSessionId.value = secret.client.sessionId;
    mcpDialogTab.value = "config";
    void loadMcpClientConfigPreview(secret.client);
    await loadMcpConnection();
    showToast("AI 工具已创建，请立即复制令牌");
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    isMcpClientSaving.value = false;
  }
}

/** 保存对现有 AI 工具的修改：名称、权限、项目范围、有效期。不改变 Token 明文。 */
async function saveMcpClient(): Promise<void> {
  const client = editingMcpClient.value;
  if (!client || isMcpClientSaving.value) return;
  const name = mcpClientDraft.value.displayName.trim();
  if (!name) {
    errorMessage.value = "请填写 AI 工具名";
    return;
  }
  isMcpClientSaving.value = true;
  try {
    const updated = await apiFetch<McpClientCard>(
      `/api/mcp/clients/${client.sessionId}`,
      {
        method: "PATCH",
        body: JSON.stringify({
          displayName: name,
          permission: mcpClientDraft.value.permission,
          projectId: mcpClientDraft.value.projectId || null,
          expiresAt: createTokenExpiration(mcpClientDraft.value.validityDays),
          clearExpiresAt: mcpClientDraft.value.validityDays === 0,
        }),
      },
      null,
    );
    editingMcpClient.value = updated;
    await loadMcpConnection();
    showToast("修改已保存");
    closeMcpClientDialog();
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    isMcpClientSaving.value = false;
  }
}

/** 轮换会话 ID：旧 `--session-id` 立即失效，生成新会话 ID；名称/权限/令牌保留。 */
async function rotateMcpClientSessionId(): Promise<void> {
  const client = editingMcpClient.value;
  if (!client || isMcpClientSaving.value) return;
  if (!window.confirm(`轮换「${client.displayName}」的会话 ID？\n旧会话 ID 将立即失效，该 AI 工具的接入配置需要重新写入（或手动更新 args 中的会话 ID）。`)) {
    return;
  }
  isMcpClientSaving.value = true;
  try {
    const rotated = await apiFetch<McpClientCard>(
      `/api/mcp/clients/${client.sessionId}/rotate`,
      { method: "POST", body: "{}" },
      null,
    );
    editingMcpClient.value = rotated;
    selectedMcpClientSessionId.value = rotated.sessionId;
    mcpConfigReport.value = null;
    await loadMcpConnection();
    // 配置预览携带新会话 ID，自动刷新供用户重新写入。
    void loadMcpClientConfigPreview(rotated);
    showToast("会话 ID 已轮换，请重新写入接入配置");
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    isMcpClientSaving.value = false;
  }
}

/** 删除当前 AI 工具：吊销并移除整个客户端会话卡片。 */
async function revokeMcpClient(): Promise<void> {
  const client = editingMcpClient.value;
  if (!client || isMcpClientSaving.value) return;
  if (!window.confirm(`删除「${client.displayName}」的 AI 工具？\n此操作会立即吊销令牌并删除该 AI 工具卡片，之后无法调用，需重新创建。`)) {
    return;
  }
  isMcpClientSaving.value = true;
  try {
    await apiFetch<{ deleted: boolean }>(
      `/api/mcp/clients/${client.sessionId}`,
      {
        method: "DELETE",
        headers: { "X-Confirm-Delete": "DELETE_MCP_CLIENT" },
      },
      null,
    );
    await loadMcpConnection();
    showToast("MCP Token 已吊销");
    closeMcpClientDialog();
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    isMcpClientSaving.value = false;
  }
}

/** 对客户端会话当前令牌执行真实 stdio 握手验收（initialize + tools/list）。 */
async function testMcpClient(client: McpClientCard): Promise<void> {
  try {
    const result = await apiFetch<McpConnectionTestResult>(
      `/api/mcp/clients/${client.sessionId}/test`,
      { method: "POST", body: "{}" },
      null,
    );
    showToast(`${result.message}（${result.toolCount} 个工具）`);
    await loadMcpConnection();
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 从后端加载 stdio 接入配置预览（含真实 exe 路径与会话 GUID，不落盘）。 */
async function loadMcpClientConfigPreview(client: McpClientCard): Promise<void> {
  isMcpConfigLoading.value = true;
  try {
    mcpConfigReport.value = await apiFetch<RegistrationReport>(
      `/api/mcp/clients/${client.sessionId}/config-preview`,
      { method: "POST", body: JSON.stringify({ clientType: client.displayName }) },
      null,
    );
  } catch (error) {
    errorMessage.value = readError(error);
    mcpConfigReport.value = null;
  } finally {
    isMcpConfigLoading.value = false;
  }
}

/** 把 stdio 接入配置写入对应 AI 客户端的配置文件（后端先备份再结构化写入并重读验证）。 */
async function registerMcpClientConfig(): Promise<void> {
  const client = editingMcpClient.value;
  if (!client || isMcpRegistering.value) return;
  const report = mcpConfigReport.value;
  if (report && report.supported && !window.confirm(
    `将把 MCP 配置写入：\n${report.configPath}\n\n已存在的配置文件会先自动备份，继续吗？`,
  )) {
    return;
  }
  isMcpRegistering.value = true;
  try {
    const result = await apiFetch<RegistrationReport>(
      `/api/mcp/clients/${client.sessionId}/register`,
      { method: "POST", body: JSON.stringify({ clientType: client.displayName }) },
      null,
    );
    mcpConfigReport.value = result;
    showToast(result.message);
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    isMcpRegistering.value = false;
  }
}

/** 复制后端生成的接入配置文本。 */
async function copyMcpClientConfig(): Promise<void> {
  const text = mcpConfigReport.value?.configText;
  if (!text) return;
  await copyMcpValue(text, "配置已复制");
}

/** 返回当前编辑或选中的客户端会话视图，供弹窗接入配置标签使用。 */
const activeMcpClientForConfig = computed<McpClientCard | null>(() => {
  if (editingMcpClient.value) return editingMcpClient.value;
  if (selectedMcpClientSessionId.value) {
    return mcpClients.value.find((c) => c.sessionId === selectedMcpClientSessionId.value) ?? null;
  }
  return null;
});

/** 返回当前接入配置标签下可复制的 stdio 配置文本。 */
const activeMcpConfigText = computed(() => mcpConfigReport.value?.configText ?? "");

/** 返回指定客户端卡片的展示状态文本。 */
function mcpClientStatusText(client: McpClientCard): string {
  if (client.status === "Revoked") return "未启用";
  if (client.status === "Expired") return "已过期";
  if (!client.tokenPrefix) return "尚未生成令牌";
  if (client.lastUsedAt) return "最近有调用";
  return "尚未调用";
}

/** 返回指定客户端卡片的展示状态对应的视觉类名。 */
function mcpClientStatusClass(client: McpClientCard): string {
  if (client.status === "Revoked") return "tone-danger";
  if (client.status === "Expired") return "tone-warning";
  if (!client.tokenPrefix) return "tone-muted";
  if (client.lastUsedAt) return "tone-mint";
  return "tone-amber";
}

/** 返回指定客户端卡片的有效期展示文本。 */
function mcpClientExpiresText(client: McpClientCard): string {
  if (!client.expiresAt) return "长期有效";
  const expires = new Date(client.expiresAt).getTime();
  if (expires <= Date.now()) return "已过期";
  return `到期：${new Date(client.expiresAt).toLocaleString("zh-CN", { hour12: false })}`;
}

/** 返回指定客户端卡片的最近调用展示文本。 */
function mcpClientUsageText(client: McpClientCard): string {
  if (!client.lastUsedAt) return "尚未调用";
  const date = new Date(client.lastUsedAt);
  const diff = Date.now() - date.getTime();
  if (diff < 60_000) return "刚刚调用";
  if (diff < 3600_000) return `${Math.floor(diff / 60_000)} 分钟前调用`;
  if (diff < 86400_000) return `${Math.floor(diff / 3600_000)} 小时前调用`;
  return `${date.toLocaleDateString("zh-CN")} 调用`;
}

/** 返回指定客户端卡片的项目范围展示文本。 */
function mcpClientScopeText(client: McpClientCard): string {
  return client.projectName ?? "全部项目";
}

/** 复制 MCP 连接值并显示轻量反馈。 */
async function copyMcpValue(value: string, message: string): Promise<void> {
  await navigator.clipboard.writeText(value);
  showToast(message);
}

/** 切换并保存明暗主题。 */
function toggleTheme(): void {
  isDarkTheme.value = !isDarkTheme.value;
  window.localStorage.setItem("theme", isDarkTheme.value ? "dark" : "light");
}

/** 显示一条会自动消失的轻量反馈。 */
function showToast(message: string): void {
  toastMessage.value = message;
  window.setTimeout(() => {
    if (toastMessage.value === message) toastMessage.value = "";
  }, 3000);
}

/** 加载总览真实聚合数据（含活动文案与 AI 工具轨道）。 */
async function loadOverview(): Promise<void> {
  try {
    overview.value = await apiFetch<OverviewResult>(
      "/api/overview",
      { method: "GET" },
      null,
    );
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 相对时间文案：刚刚 / N 分钟前 / N 小时前 / N 天前。 */
function formatRelativeTime(iso: string): string {
  const timestamp = new Date(iso).getTime();
  if (!Number.isFinite(timestamp)) return "";
  const seconds = Math.max(0, (Date.now() - timestamp) / 1000);
  if (seconds < 60) return "刚刚";
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} 小时前`;
  return `${Math.floor(seconds / 86400)} 天前`;
}

/** 将 MCP 活动类型转换为总览中的中文动作。 */
function mcpActivityActionText(action: "READ" | "CREATE" | "UPDATE" | "ARCHIVE"): string {
  switch (action) {
    case "CREATE":
      return "创建了";
    case "UPDATE":
      return "更新了";
    case "ARCHIVE":
      return "归档了";
    case "READ":
      return "读取了";
  }
}

/** MCP 活动文案：如「Codex 刚刚更新了项目记忆」。 */
const overviewActivityText = computed(() => {
  const activity = overview.value?.mcpActivity;
  if (!activity) {
    return "保存个人偏好、项目决策和长期经验，通过中文与语义混合检索快速找回。";
  }
  const action = mcpActivityActionText(activity.action);
  const scope =
    activity.scope === "Mixed"
      ? "项目与个人记忆"
      : activity.scope === "Project"
        ? "项目记忆"
        : "个人记忆";
  return `${activity.displayName} ${formatRelativeTime(activity.occurredAt)}${action}${scope}。`;
});

/** 总览轨道 AI 工具（最多 6 个，双层分布）与「+N」。 */
const orbitClients = computed(() => overview.value?.recentClients ?? []);
const orbitExtraCount = computed(() =>
  Math.max(0, (overview.value?.activeClientCount ?? 0) - orbitClients.value.length),
);

/** 轨道节点位置：双层轨道按角度分布。 */
function orbitStyle(index: number, ring: "inner" | "outer"): Record<string, string> {
  const radius = ring === "inner" ? 84 : 120;
  const seats = ring === "inner" ? 3 : 3;
  const offset = ring === "inner" ? -90 : -30;
  const angle = offset + (360 / seats) * ((index % seats) + 0);
  return {
    transform: `rotate(${angle}deg) translateX(${radius}px) rotate(${-angle}deg)`,
  };
}

/** 加载总览图谱预览（24 节点真实关系云）。 */
async function loadOverviewGraphPreview(): Promise<void> {
  try {
    const params = new URLSearchParams({ limit: "24", minScore: "0.3" });
    overviewGraph.value = await apiFetch<GraphResult>(
      `/api/graph/global?${params}`,
      { method: "GET" },
      null,
    );
    await nextTick();
    renderOverviewGraphPreview();
  } catch {
    overviewGraph.value = null;
  }
}

/** 预览节点配色：项目色 / 个人紫。 */
function graphPreviewColor(memory: { scope: string; projectId: string | null }): string {
  if (memory.scope === "Personal" || !memory.projectId) {
    return isDarkTheme.value ? "#a48ade" : "#7c5cb8";
  }
  return (
    activeProjects.value.find((project) => project.id === memory.projectId)?.color
      ?? (isDarkTheme.value ? "#7188dd" : "#4a65c7")
  );
}

function renderOverviewGraphPreview(): void {
  const canvas = overviewGraphCanvas.value;
  if (!canvas || !overviewGraph.value) return;
  renderGraphPreview(
    canvas,
    overviewGraph.value.nodes,
    overviewGraph.value.edges,
    {
      isDark: isDarkTheme.value,
      colorOf: graphPreviewColor,
    },
  );
}

/** 从图谱打开完整记忆详情。 */
async function openMemoryFromGraph(memoryId: string): Promise<void> {
  try {
    const memory = await apiFetch<MemoryItem>(
      `/api/memories/${memoryId}`,
      { method: "GET" },
      null,
    );
    selectedMemory.value = memory;
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 从记忆详情进入以该记忆为圆心的两跳局部图谱。 */
function openGraphFromMemory(memory: MemoryItem): void {
  graphEntryMemoryId.value = memory.id;
  void selectNavigation("graph");
}

/** 图谱「搜索相关记忆」：带回检索词并切换到记忆页。 */
function openGraphSearch(query: string): void {
  if (query.trim()) {
    searchQuery.value = query.trim();
  }
  void selectNavigation("memories");
}

/** 加载项目筛选项。 */
async function loadProjects(): Promise<void> {
  projects.value = await apiFetch<ProjectItem[]>(
    "/api/projects?includeArchived=true",
    { method: "GET" },
    null,
  );
}

/** 加载记忆分类及各项目数量。 */
async function loadMemoryFacets(): Promise<void> {
  memoryFacets.value = await apiFetch<MemoryFacets>(
    "/api/memories/facets",
    { method: "GET" },
    null,
  );
}

/** 保存总览快速记录。 */
async function saveQuickCapture(): Promise<void> {
  if (!quickCapture.value.trim() || isSaving.value) return;
  isSaving.value = true;
  try {
    const memory = await apiFetch<MemoryItem>(
      "/api/memories/quick-capture",
      { method: "POST", body: JSON.stringify({ content: quickCapture.value }) },
      null,
    );
    quickCapture.value = "";
    selectedMemory.value = memory;
    showToast("记忆已保存到本机");
    await loadOverview();
  } catch (error) {
    if (error instanceof ApiRequestError && error.code === "MEMORY_DUPLICATE") {
      const title = quickCapture.value.trim().split(/\r?\n/, 1)[0].slice(0, 60);
      const existing = await apiFetch<SearchResult[]>(
        "/api/search",
        {
          method: "POST",
          body: JSON.stringify({
            query: title,
            scope: "Personal",
            projectId: null,
            type: null,
            tag: null,
            limit: 5,
            semanticEnabled: false,
          }),
        },
        null,
      );
      const exact = existing.find(
        (result) => result.memory.content.trim() === quickCapture.value.trim(),
      );
      if (exact) {
        await selectMemory(exact.memory);
        showToast("已打开内容相同的记忆");
        return;
      }
    }
    errorMessage.value = readError(error);
  } finally {
    isSaving.value = false;
  }
}

/** 延迟执行搜索并取消旧请求。 */
function scheduleSearch(): void {
  if (searchTimer !== null) window.clearTimeout(searchTimer);
  isSearching.value = true;
  searchTimer = window.setTimeout(runSearch, 250);
}

/** 执行全局混合搜索。 */
async function runSearch(): Promise<void> {
  const query = searchQuery.value.trim();
  searchController?.abort();
  if (!query) {
    searchResults.value = [];
    isSearching.value = false;
    return;
  }
  const controller = new AbortController();
  searchController = controller;
  try {
    const results = await apiFetch<SearchResult[]>(
      "/api/search",
      {
        method: "POST",
        body: JSON.stringify({
          query,
          scope: searchScope(),
          projectId:
            activeNavigation.value === "memories" &&
            memoryView.value === "PROJECT"
              ? selectedProjectId.value
              : null,
          type: null,
          tag: null,
          limit: 20,
          semanticEnabled: true,
        }),
      },
      controller.signal,
    );
    if (searchController === controller) searchResults.value = results;
  } catch (error) {
    if (!controller.signal.aborted) errorMessage.value = readError(error);
  } finally {
    // 仅当前请求可结束搜索态；被新请求取代的旧请求不覆盖状态。
    if (searchController === controller) isSearching.value = false;
  }
}

/** 返回当前记忆分类对应的检索范围。 */
function searchScope(): "Personal" | "Project" | null {
  if (activeNavigation.value !== "memories") return null;
  if (memoryView.value === "PERSONAL") return "Personal";
  if (memoryView.value === "PROJECT_ALL" || memoryView.value === "PROJECT")
    return "Project";
  return null;
}

/** 选择一个清晰的记忆分类并刷新列表。 */
function selectMemoryView(view: MemoryViewKey, projectId: string): void {
  memoryView.value = view;
  selectedProjectId.value = projectId;
  searchResults.value = [];
  // 搜索中切换分类：范围随分类变化，必须重新执行搜索，否则结果被清空后不再恢复。
  if (searchQuery.value.trim()) scheduleSearch();
}

/** 加载全部待确认候选。 */
async function loadCandidates(): Promise<void> {
  try {
    candidates.value = await apiFetch<MemoryCandidateItem[]>(
      "/api/memory-candidates",
      { method: "GET" },
      null,
    );
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 识别候选类型并打开对应的单一职责编辑器。 */
async function openCandidateEditor(candidate: MemoryCandidateItem): Promise<void> {
  editingCandidateId.value = candidate.id;
  editingConclusionCandidate.value = false;
  isLoadingCandidateStructure.value = true;
  memoryForm.value = {
    id: candidate.id,
    scope: candidate.scope,
    projectId: candidate.projectId ?? "",
    title: candidate.title,
    summary: candidate.summary,
    content: candidate.content,
    memoryType: candidate.memoryType,
    keywords: candidate.keywords.join(", "),
    tags: candidate.tags.join(", "),
    importance: candidate.importance,
    cloudProcessingAllowed: candidate.cloudProcessingAllowed,
    expectedVersion: candidate.version,
  };
  showMemoryEditor.value = true;
  try {
    const payload = await apiFetch<ConclusionCardPayload | null>(
      `/api/memory-candidates/${candidate.id}/conclusion-payload`,
      { method: "GET" },
      null,
    );
    editingConclusionCandidate.value = payload !== null;
  } catch (error) {
    errorMessage.value = readError(error);
    closeMemoryEditor();
  } finally {
    isLoadingCandidateStructure.value = false;
  }
}

/** 用结构化编辑结果刷新候选版本，保证后续保存继续使用最新乐观锁。 */
function handleConclusionCandidateUpdated(candidate: MemoryCandidateItem): void {
  candidates.value = candidates.value.map((item) =>
    item.id === candidate.id ? candidate : item,
  );
  memoryForm.value.expectedVersion = candidate.version;
}

/** 使用乐观锁确认候选并刷新全部相关页面。 */
async function confirmCandidate(candidate: MemoryCandidateItem): Promise<void> {
  try {
    await apiFetch<MemoryItem>(
      `/api/memory-candidates/${candidate.id}/confirm`,
      {
        method: "POST",
        body: JSON.stringify({ expectedVersion: candidate.version }),
      },
      null,
    );
    await Promise.all([
      loadCandidates(),
      loadOverview(),
      loadMemoryFacets(),
      loadProjects(),
    ]);
    showToast("候选已确认并进入正式记忆");
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 使用乐观锁拒绝候选。 */
async function rejectCandidate(candidate: MemoryCandidateItem): Promise<void> {
  try {
    await apiFetch<void>(
      `/api/memory-candidates/${candidate.id}/reject`,
      {
        method: "POST",
        body: JSON.stringify({ expectedVersion: candidate.version }),
      },
      null,
    );
    await Promise.all([loadCandidates(), loadOverview()]);
    showToast("候选已拒绝");
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 打开新建记忆表单。 */
function openNewMemory(): void {
  closeMemoryDetails();
  editingCandidateId.value = null;
  memoryForm.value = createEmptyMemoryForm();
  if (memoryView.value === "PROJECT" && selectedProjectId.value) {
    memoryForm.value.scope = "Project";
    memoryForm.value.projectId = selectedProjectId.value;
  }
  showMemoryEditor.value = true;
}

/** 打开记忆详情。 */
function selectMemory(memory: MemoryItem): void {
  selectedMemory.value = memory;
}

/** 重新读取修改后的记忆及所有关联页面数据，避免局部状态停留在修改前。 */
async function refreshMemoryState(memoryId: string): Promise<void> {
  await Promise.all([
    loadMemories(false),
    loadMemoryFacets(),
    loadProjects(),
    loadOverview(),
  ]);
  const current = await apiFetch<MemoryItem>(
    `/api/memories/${memoryId}`,
    { method: "GET" },
    null,
  );
  selectMemory(current);
}

/** 打开已有记忆编辑表单。 */
function openMemoryEditor(memory: MemoryItem): void {
  if (memory.status === "Archived") return;
  editingCandidateId.value = null;
  selectedMemory.value = memory;
  memoryForm.value = {
    id: memory.id,
    scope: memory.scope,
    projectId: memory.projectId ?? "",
    title: memory.title,
    summary: memory.summary,
    content: memory.content,
    memoryType: memory.memoryType,
    keywords: memory.keywords.join(", "),
    tags: memory.tags.join(", "),
    importance: memory.importance,
    cloudProcessingAllowed: memory.cloudProcessingAllowed,
    expectedVersion: memory.version,
  };
  showMemoryEditor.value = true;
}

/** 关闭记忆编辑器并清除候选编辑身份。 */
function closeMemoryEditor(): void {
  showMemoryEditor.value = false;
  editingCandidateId.value = null;
  editingConclusionCandidate.value = false;
  isLoadingCandidateStructure.value = false;
}

/** 保存新建或修改的记忆。 */
async function saveMemory(): Promise<void> {
  if (isSaving.value) return;
  const form = memoryForm.value;
  isSaving.value = true;
  try {
    const bodyObject = {
      scope: form.scope,
      projectId: form.scope === "Project" ? form.projectId : null,
      title: form.title,
      summary: form.summary,
      content: form.content,
      memoryType: form.memoryType,
      keywords: splitValues(form.keywords),
      tags: splitValues(form.tags),
      importance: form.importance,
      isFavorite: selectedMemory.value?.isFavorite ?? false,
      isPinned: selectedMemory.value?.isPinned ?? false,
      cloudProcessingAllowed: form.cloudProcessingAllowed,
      expectedVersion: form.expectedVersion,
    };
    const body = JSON.stringify(bodyObject);
    if (editingCandidateId.value) {
      await apiFetch<MemoryCandidateItem>(
        `/api/memory-candidates/${editingCandidateId.value}`,
        {
          method: "PUT",
          body: JSON.stringify({
            scope: bodyObject.scope,
            projectId: bodyObject.projectId,
            title: bodyObject.title,
            summary: bodyObject.summary,
            content: bodyObject.content,
            memoryType: bodyObject.memoryType,
            keywords: bodyObject.keywords,
            tags: bodyObject.tags,
            importance: bodyObject.importance,
            cloudProcessingAllowed: bodyObject.cloudProcessingAllowed,
            expectedVersion: bodyObject.expectedVersion,
          }),
        },
        null,
      );
      editingCandidateId.value = null;
      showMemoryEditor.value = false;
      await loadCandidates();
      showToast("候选已更新");
      return;
    }
    const path = form.id ? `/api/memories/${form.id}` : "/api/memories";
    const memory = await apiFetch<MemoryItem>(
      path,
      { method: form.id ? "PUT" : "POST", body },
      null,
    );
    showMemoryEditor.value = false;
    await refreshMemoryState(memory.id);
    showToast(form.id ? "记忆已更新，页面已同步" : "记忆已创建");
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    isSaving.value = false;
  }
}

/** 加载记忆列表，按需追加下一页。 */
async function loadMemories(append: boolean, silent = false): Promise<void> {
  if (memoryView.value === "PENDING") {
    await loadCandidates();
    return;
  }
  // 静默刷新（data_version 探测/手动刷新）不切换加载态，避免列表闪"正在读取"。
  if (!silent) isLoading.value = true;
  try {
    const cursor = append ? nextCursor.value : null;
    const params = new URLSearchParams({
      status: memoryView.value === "ARCHIVED" ? "Archived" : "Active",
      size: "30",
    });
    if (memoryView.value === "PERSONAL") params.set("scope", "Personal");
    if (memoryView.value === "PROJECT_ALL" || memoryView.value === "PROJECT")
      params.set("scope", "Project");
    if (memoryView.value === "PROJECT" && selectedProjectId.value)
      params.set("projectId", selectedProjectId.value);
    if (memoryView.value === "FAVORITE") params.set("favorite", "true");
    if (memoryView.value === "PINNED") params.set("pinned", "true");
    if (listType.value) params.set("type", listType.value);
    if (listTag.value.trim()) params.set("tag", listTag.value.trim());
    if (listImportance.value > 1)
      params.set("importanceMin", String(listImportance.value));
    if (cursor) params.set("cursor", cursor);
    const page = await apiFetch<CursorPage<MemoryItem>>(
      `/api/memories?${params.toString()}`,
      { method: "GET" },
      null,
    );
    memories.value = append ? memories.value.concat(page.items) : page.items;
    nextCursor.value = page.nextCursor;
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    if (!silent) isLoading.value = false;
  }
}

/** 更新收藏或置顶状态，不改变记忆正文。 */
async function updateMemoryMarkers(
  memory: MemoryItem,
  isFavorite: boolean,
  isPinned: boolean,
): Promise<void> {
  try {
    const updated = await apiFetch<MemoryItem>(
      `/api/memories/${memory.id}`,
      {
        method: "PUT",
        body: JSON.stringify({
          scope: memory.scope,
          projectId: memory.projectId,
          title: memory.title,
          summary: memory.summary,
          content: memory.content,
          memoryType: memory.memoryType,
          keywords: memory.keywords,
          tags: memory.tags,
          importance: memory.importance,
          isFavorite,
          isPinned,
          cloudProcessingAllowed: memory.cloudProcessingAllowed,
          expectedVersion: memory.version,
        }),
      },
      null,
    );
    await refreshMemoryState(updated.id);
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 打开记忆归档确认弹窗。 */
function openArchiveConfirmation(memory: MemoryItem): void {
  if (memory.status !== "Active") return;
  archiveConfirmationMemory.value = memory;
}

/** 关闭记忆归档确认弹窗。 */
function closeArchiveConfirmation(): void {
  if (isConfirmingMemoryAction.value) return;
  archiveConfirmationMemory.value = null;
}

/** 确认归档当前记忆。 */
async function confirmArchiveMemory(): Promise<void> {
  const memory = archiveConfirmationMemory.value;
  if (!memory || isConfirmingMemoryAction.value) return;
  isConfirmingMemoryAction.value = true;
  try {
    await apiFetch<MemoryItem>(
      `/api/memories/${memory.id}/archive`,
      { method: "POST" },
      null,
    );
    archiveConfirmationMemory.value = null;
    selectedMemory.value = null;
    showToast("记忆已归档");
    await Promise.all([
      loadMemories(false),
      loadMemoryFacets(),
      loadProjects(),
      loadOverview(),
    ]);
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    isConfirmingMemoryAction.value = false;
  }
}

/** 恢复一条已归档记忆。 */
async function restoreArchivedMemory(memory: MemoryItem): Promise<void> {
  if (memory.status !== "Archived") return;
  try {
    await apiFetch<MemoryItem>(
      `/api/memories/${memory.id}/restore`,
      { method: "POST" },
      null,
    );
    selectedMemory.value = null;
    showToast("记忆已恢复");
    await Promise.all([
      loadMemories(false),
      loadMemoryFacets(),
      loadProjects(),
      loadOverview(),
    ]);
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 打开永久删除确认弹窗。 */
function openDeleteConfirmation(memory: MemoryItem): void {
  if (memory.status !== "Archived") return;
  deleteConfirmationMemory.value = memory;
}

/** 关闭永久删除确认弹窗。 */
function closeDeleteConfirmation(): void {
  if (isConfirmingMemoryAction.value) return;
  deleteConfirmationMemory.value = null;
}

/** 确认永久删除当前归档记忆。 */
async function confirmPermanentDelete(): Promise<void> {
  const memory = deleteConfirmationMemory.value;
  if (!memory || isConfirmingMemoryAction.value) return;
  isConfirmingMemoryAction.value = true;
  try {
    await apiFetch<{ deleted: boolean }>(
      `/api/memories/${memory.id}/permanent`,
      { method: "DELETE", headers: { "X-Confirm-Delete": "DELETE_MEMORY" } },
      null,
    );
    deleteConfirmationMemory.value = null;
    closeMemoryDetails();
    await Promise.all([
      loadMemories(false),
      loadMemoryFacets(),
      loadProjects(),
      loadOverview(),
    ]);
    showToast("记忆已彻底删除");
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    isConfirmingMemoryAction.value = false;
  }
}

/** 打开新建项目表单。 */
function openNewProject(): void {
  editingProjectId.value = null;
  projectName.value = "";
  projectDescription.value = "";
  projectColor.value = "#4f8cff";
  projectWorkspaceIdentifier.value = "";
  showProjectEditor.value = true;
}

/** 打开项目编辑表单。 */
function openProjectEditor(project: ProjectItem): void {
  editingProjectId.value = project.id;
  projectName.value = project.name;
  projectDescription.value = project.description;
  projectColor.value = project.color;
  projectWorkspaceIdentifier.value = project.workspaceIdentifier ?? "";
  showProjectEditor.value = true;
}

/** 创建或修改一个项目。工作空间标识必填：为空直接拦截，不发起请求。 */
async function saveProject(): Promise<void> {
  // 新建项目时若工作空间绑定失败（如标识已被活动项目占用），
  // 回滚删除刚创建的空项目，避免留下"无绑定标识"的项目。
  const identifier = projectWorkspaceIdentifier.value.trim();
  if (!identifier) {
    errorMessage.value = "工作空间标识为必填项，请填写后再保存";
    return;
  }
  const isNewProject = !editingProjectId.value;
  try {
    const path = editingProjectId.value
      ? `/api/projects/${editingProjectId.value}`
      : "/api/projects";
    const project = await apiFetch<ProjectItem>(
      path,
      {
        method: editingProjectId.value ? "PUT" : "POST",
        body: JSON.stringify({
          name: projectName.value,
          description: projectDescription.value,
          color: projectColor.value,
        }),
      },
      null,
    );
    try {
      await apiFetch<ProjectItem>(
        `/api/projects/${project.id}/workspace`,
        {
          method: "PUT",
          body: JSON.stringify({ workspaceIdentifier: identifier }),
        },
        null,
      );
    } catch (bindError) {
      if (isNewProject) {
        await apiFetch<ProjectItem>(
          `/api/projects/${project.id}/archive`,
          { method: "POST" },
          null,
        ).catch(() => undefined);
        await apiFetch<DeletedProjectStats>(
          `/api/projects/${project.id}`,
          { method: "DELETE" },
          null,
        ).catch(() => undefined);
      }
      throw bindError;
    }
    showProjectEditor.value = false;
    await Promise.all([loadProjects(), loadMemoryFacets(), loadOverview()]);
    showToast(editingProjectId.value ? "项目已更新" : "项目已创建");
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 归档当前编辑的项目：弹出居中确认框（有记忆时提示将同步归档）。 */
function archiveProject(): void {
  if (!editingProjectId.value) return;
  const project = projects.value.find((item) => item.id === editingProjectId.value);
  if (!project) return;
  projectConfirmation.value = {
    kind: "archive",
    project,
    memoryCount: memoryFacets.value.projectCounts[project.id] ?? 0,
  };
}

/** 彻底删除已归档项目：弹出居中确认框（提示记忆数，不可恢复）。 */
function deleteProjectPermanently(project: ProjectItem): void {
  projectConfirmation.value = {
    kind: "delete",
    project,
    memoryCount: project.totalMemoryCount,
  };
}

/** 关闭项目确认弹框。 */
function closeProjectConfirmation(): void {
  if (!isProjectActionRunning.value) projectConfirmation.value = null;
}

/** 确认执行项目归档/彻底删除。 */
async function confirmProjectAction(): Promise<void> {
  const confirmation = projectConfirmation.value;
  if (!confirmation || isProjectActionRunning.value) return;
  isProjectActionRunning.value = true;
  try {
    if (confirmation.kind === "archive") {
      await apiFetch<ProjectItem>(
        `/api/projects/${confirmation.project.id}/archive`,
        { method: "POST" },
        null,
      );
      if (selectedProjectId.value === confirmation.project.id)
        selectedProjectId.value = "";
      showProjectEditor.value = false;
      await Promise.all([
        loadProjects(),
        loadMemories(false),
        loadMemoryFacets(),
        loadOverview(),
      ]);
      showToast("项目已归档");
    } else {
      const stats = await apiFetch<DeletedProjectStats>(
        `/api/projects/${confirmation.project.id}`,
        { method: "DELETE" },
        null,
      );
      await Promise.all([
        loadProjects(),
        loadMemories(false),
        loadMemoryFacets(),
        loadOverview(),
      ]);
      showToast(
        stats.deletedMemories
          ? `项目已彻底删除（含 ${stats.deletedMemories} 条记忆）`
          : "项目已彻底删除",
      );
    }
    projectConfirmation.value = null;
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    isProjectActionRunning.value = false;
  }
}

/** 展开/收起已归档项目卡片的记忆列表（手风琴）。 */
function toggleArchivedProject(projectId: string): void {
  expandedArchivedProjectId.value =
    expandedArchivedProjectId.value === projectId ? null : projectId;
}

/** 已归档项目内的记忆（ARCHIVED 视图已加载 status=Archived 的全量分页）。 */
function archivedProjectMemories(projectId: string): MemoryItem[] {
  return memories.value.filter((memory) => memory.projectId === projectId);
}

/** 恢复一个已归档项目，使其重新接受项目记忆（已归档记忆保持归档）。 */
async function restoreProject(project: ProjectItem): Promise<void> {
  try {
    await apiFetch<ProjectItem>(
      `/api/projects/${project.id}/restore`,
      { method: "POST" },
      null,
    );
    await Promise.all([loadProjects(), loadOverview()]);
    showToast("项目已恢复，其已归档记忆仍在「已归档」中");
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 加载 Embedding 配置（回到雾化展示态）。 */
async function loadEmbeddingSettings(): Promise<void> {
  try {
    embedding.value = await apiFetch<EmbeddingSettings>(
      "/api/settings/embedding",
      { method: "GET" },
      null,
    );
    cancelEmbeddingKeyEdit();
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 测试并保存 Embedding 配置。 */
async function saveEmbeddingSettings(): Promise<void> {
  // 编辑态用 embeddingApiKey，雾化展示态用已保存的 embedding.apiKey
  const actualApiKey = isEmbeddingKeyEditing.value
    ? embeddingApiKey.value
    : embedding.value.apiKey;
  if (!actualApiKey.trim()) {
    errorMessage.value = "请填写 API Key";
    return;
  }
  isSaving.value = true;
  try {
    const body = JSON.stringify({
      baseUrl: embedding.value.baseUrl,
      model: embedding.value.model,
      apiKey: actualApiKey,
      dimensions: embedding.value.dimensions,
      enabled: embedding.value.enabled,
    });
    embedding.value = await apiFetch<EmbeddingSettings>(
      "/api/settings/embedding",
      { method: "PUT", body },
      null,
    );
    cancelEmbeddingKeyEdit();
    showToast("Embedding 配置已验证并保存");
  } catch (error) {
    errorMessage.value = readError(error);
  } finally {
    isSaving.value = false;
  }
}

/** 请求重新生成全部记忆向量。 */
async function rebuildEmbeddings(): Promise<void> {
  try {
    await apiFetch<object>(
      "/api/settings/embedding/rebuild",
      { method: "POST" },
      null,
    );
    showToast("已在后台开始重建向量");
  } catch (error) {
    errorMessage.value = readError(error);
  }
}

/** 处理全局快捷键。 */
function handleShortcut(event: KeyboardEvent): void {
  if (event.ctrlKey && event.key.toLowerCase() === "n") {
    event.preventDefault();
    openNewMemory();
  }
  if (event.key === "Escape") {
    if (document.activeElement instanceof HTMLElement)
      document.activeElement.blur();
    clearMcpSecrets();
    closeMemoryDetails();
    closeMemoryEditor();
    showProjectEditor.value = false;
    closeArchiveConfirmation();
    closeDeleteConfirmation();
    if (showMcpClientDialog.value) closeMcpClientDialog();
  }
}

/** 分割用户输入的标签或关键词。 */
function splitValues(value: string): string[] {
  return value
    .split(/[,，]/)
    .map((item) => item.trim())
    .filter(Boolean);
}

/** 将未知异常转换为可读文本。 */
function readError(error: unknown): string {
  return error instanceof Error ? error.message : "操作失败，请重试";
}

watch(memoryView, () => loadMemories(false));
watch([listType, listImportance], () => loadMemories(false));
watch(listTag, () => {
  window.setTimeout(() => loadMemories(false), 250);
});
watch(selectedProjectId, () => {
  searchResults.value = [];
  loadMemories(false);
  if (searchQuery.value.trim()) scheduleSearch();
});
watch(searchQuery, scheduleSearch);
watch(activeNavigation, (current, previous) => {
  if (previous === "connections" && current !== "connections") {
    issuedMcpClientToken.value = null;
    if (showMcpClientDialog.value) closeMcpClientDialog();
  }
  if (previous === "graph" && current !== "graph") {
    // 离开图谱页后清除入口记忆，下次进入默认全局图谱。
    graphEntryMemoryId.value = null;
  }
});
// 主题切换时重绘总览预览（配色跟随明暗）。
watch(isDarkTheme, () => {
  if (activeNavigation.value === "overview") {
    renderOverviewGraphPreview();
  }
});

/** 窗口失焦时清除前端持有的 Token 明文。 */
function clearMcpSecrets(): void {
  issuedMcpClientToken.value = null;
}

onMounted(async () => {
  window.addEventListener("keydown", handleShortcut);
  window.addEventListener("blur", clearMcpSecrets);
  window.addEventListener("visibilitychange", handleVisibilityChange);
  window.addEventListener("resize", renderOverviewGraphPreview);
  await Promise.all([loadOverview(), loadProjects(), loadMemoryFacets()]);
  if (GRAPH_FEATURE_ENABLED) void loadOverviewGraphPreview();
  startAutoRefresh();
});

onBeforeUnmount(() => {
  window.removeEventListener("keydown", handleShortcut);
  window.removeEventListener("blur", clearMcpSecrets);
  window.removeEventListener("visibilitychange", handleVisibilityChange);
  window.removeEventListener("resize", renderOverviewGraphPreview);
  stopAutoRefresh();
  searchController?.abort();
  if (searchTimer !== null) window.clearTimeout(searchTimer);
  if (apiKeyClickTimer !== null) window.clearTimeout(apiKeyClickTimer);
});

/** 数据版本探测：每秒轻查询 data_version（微秒级 PRAGMA），变化时才执行真正的
 *  数据刷新——MCP 写入约 1 秒内自动呈现，空闲时零数据查询（替代 5 秒定时全量拉取）。 */
let autoRefreshTimer: ReturnType<typeof setInterval> | null = null;
let knownDataVersion: number | null = null;
let probingDataVersion = false;
function startAutoRefresh(): void {
  if (autoRefreshTimer !== null) return;
  autoRefreshTimer = setInterval(() => void probeDataVersion(), 1000);
}
function stopAutoRefresh(): void {
  if (autoRefreshTimer !== null) {
    clearInterval(autoRefreshTimer);
    autoRefreshTimer = null;
  }
}
async function probeDataVersion(): Promise<void> {
  if (document.hidden || probingDataVersion) return;
  probingDataVersion = true;
  try {
    const version = await apiFetch<number>("/api/data-version", { method: "GET" }, null);
    if (knownDataVersion === null) {
      knownDataVersion = version;
      return;
    }
    if (version !== knownDataVersion) {
      knownDataVersion = version;
      await autoRefresh();
    }
  } catch {
    // 探测失败静默：不打扰用户，下一轮重试。
  } finally {
    probingDataVersion = false;
  }
}

/** 手动刷新：topbar 按钮触发，转圈反馈期间防重入。
 *  本地库刷新可能几十毫秒即完成，动画 0.7s/圈，
 *  故保底 700ms 让图标至少完整转一圈，点击才有可感知反馈。 */
const isRefreshing = ref(false);
async function manualRefresh(): Promise<void> {
  if (isRefreshing.value) return;
  isRefreshing.value = true;
  try {
    await Promise.all([autoRefresh(), new Promise((resolve) => setTimeout(resolve, 700))]);
  } finally {
    isRefreshing.value = false;
  }
}
function handleVisibilityChange(): void {
  isWindowHidden.value = document.hidden;
  if (document.hidden) {
    stopAutoRefresh();
  } else {
    // 延迟 400ms 再刷新数据，让 WebView2 渲染器先完成首帧绘制，避免唤醒时卡顿
    window.setTimeout(() => {
      if (!document.hidden) void autoRefresh();
    }, 400);
    startAutoRefresh();
  }
}
/** 按当前所在页面静默刷新对应数据，不弹出错误。 */
async function autoRefresh(): Promise<void> {
  if (document.hidden) return;
  try {
    const tasks: Promise<unknown>[] = [];
    tasks.push(
      apiFetch<OverviewResult>("/api/overview", { method: "GET" }, null).then(
        (data) => {
          overview.value = data;
        },
      ),
    );
    tasks.push(
      apiFetch<ProjectItem[]>("/api/projects?includeArchived=true", { method: "GET" }, null).then(
        (data) => {
          projects.value = data;
        },
      ),
    );
    if (activeNavigation.value === "connections") {
      tasks.push(
        apiFetch<McpClientCard[]>("/api/mcp/clients", { method: "GET" }, null).then(
          (data) => {
            mcpClients.value = data;
          },
        ),
      );
    }
    if (activeNavigation.value === "memories") {
      tasks.push(loadMemoryFacets());
      // 关键：重载记忆列表第一页，否则外部（MCP/AI）新写的记忆永远不出现。
      tasks.push(loadMemories(false, true));
    }
    await Promise.all(tasks);
  } catch {
    // 静默刷新失败不打扰用户
  }
}
</script>

<template>
  <main
    class="desktop-app"
    :class="{ 'theme-dark': isDarkTheme }"
    @click="closeMemoryDetailsOutside"
  >
    <aside class="sidebar" aria-label="主导航">
      <button class="brand" type="button" @click="selectNavigation('overview')">
        <span class="brand-mark" aria-hidden="true"><i></i><i></i><i></i></span>
        <span
          ><strong>MemStack</strong><small>PERSONAL MEMORY HUB</small></span
        >
      </button>
      <nav class="navigation-list">
        <button
          v-for="item in visibleNavigationItems"
          :key="item.key"
          class="navigation-item"
          :class="{ active: activeNavigation === item.key }"
          type="button"
          @click="selectNavigation(item.key)"
        >
          <span aria-hidden="true">{{ item.icon }}</span
          ><span>{{ item.label }}</span>
        </button>
      </nav>
      <section class="sidebar-status" aria-label="运行状态">
        <span class="status-dot"></span>
        <div>
          <strong>本地记忆中枢</strong
          ><small
            >SQLite ·
            {{
              overview?.searchMode === "HYBRID" ? "混合检索" : "关键词检索"
            }}</small
          >
        </div>
      </section>
    </aside>

    <section class="workspace">
      <header class="topbar">
        <div>
          <small>个人桌面客户端 / {{ pageTitle }}</small>
          <h1>{{ pageTitle }}</h1>
        </div>
        <div class="topbar-actions">
          <button
            class="theme-button"
            type="button"
            aria-label="立即刷新数据"
            title="立即刷新数据"
            :disabled="isRefreshing"
            @click="manualRefresh"
          >
            <span :class="{ 'refresh-icon': true, spinning: isRefreshing }">⟳</span></button
          ><button
            v-if="activeNavigation === 'memories'"
            class="primary-button compact"
            type="button"
            @click="openNewMemory"
          >
            ＋ 新建记忆</button
          ><button
            class="theme-button"
            type="button"
            :aria-label="isDarkTheme ? '切换浅色主题' : '切换深色主题'"
            @click="toggleTheme"
          >
            {{ isDarkTheme ? "☼" : "◐" }}
          </button>
        </div>
      </header>
      <div v-if="errorMessage" class="error-banner">
        <span>{{ errorMessage }}</span>
      </div>

      <div v-if="activeNavigation === 'overview'" class="overview-page">
        <section class="hero-grid">
          <article class="hero-card stagger-1">
            <div class="hero-copy">
              <span class="live-pill"><i></i> 本地优先 · 数据已持久化</span>
              <h2>让每一个 AI，<br /><em>记得同一个你。</em></h2>
              <p>{{ overviewActivityText }}</p>
            </div>
            <div
              class="memory-core"
              :class="{ 'orbit-paused': isWindowHidden }"
              aria-label="记忆中枢"
            >
              <span class="core-ring ring-one"></span
              ><span class="core-ring ring-two"></span
              ><span class="core-ring ring-three"></span>
              <div
                v-if="orbitClients.length"
                class="core-orbit orbit-inner"
                aria-hidden="true"
              >
                <span
                  v-for="(client, index) in orbitClients.slice(0, 3)"
                  :key="`inner-${client.displayName}`"
                  class="orbit-client"
                  :style="orbitStyle(index, 'inner')"
                >
                  <i :style="{ background: getLogoColor(client.displayName) }">{{
                    getLogoLetter(client.displayName)
                  }}</i>
                </span>
              </div>
              <div
                v-if="orbitClients.length > 3"
                class="core-orbit orbit-outer"
                aria-hidden="true"
              >
                <span
                  v-for="(client, index) in orbitClients.slice(3, 6)"
                  :key="`outer-${client.displayName}`"
                  class="orbit-client"
                  :style="orbitStyle(index, 'outer')"
                >
                  <i :style="{ background: getLogoColor(client.displayName) }">{{
                    getLogoLetter(client.displayName)
                  }}</i>
                </span>
                <span
                  v-if="orbitExtraCount > 0"
                  class="orbit-client orbit-extra"
                  :style="orbitStyle(2, 'outer')"
                >
                  <i>+{{ orbitExtraCount }}</i>
                </span>
              </div>
              <span class="core-center"
                ><b>{{ overview?.memoryCount ?? 0 }}</b
                ><small>有效记忆</small></span
              >
            </div>
          </article>
          <article class="quick-capture-card stagger-2">
            <div class="section-heading">
              <div>
                <small>QUICK CAPTURE</small>
                <h3>快速记录</h3>
              </div>
              <span>个人</span>
            </div>
            <textarea
              v-model="quickCapture"
              maxlength="200000"
              placeholder="记下一条不想忘记的内容…"
              @keydown.ctrl.enter="saveQuickCapture"
            ></textarea>
            <div class="capture-footer">
              <small>Ctrl + Enter 保存到本机 SQLite</small
              ><button
                type="button"
                :disabled="!quickCapture.trim() || isSaving"
                @click="saveQuickCapture"
              >
                {{ isSaving ? "…" : "↑" }}
              </button>
            </div>
          </article>
        </section>
        <section class="metric-grid">
          <article class="stagger-3">
            <span class="metric-icon mint">✦</span>
            <div>
              <small>长期记忆</small
              ><strong>{{ overview?.memoryCount ?? 0 }}</strong>
              <p>个人与项目记忆</p>
            </div>
          </article>
          <article class="stagger-4">
            <span class="metric-icon blue">▦</span>
            <div>
              <small>项目空间</small
              ><strong>{{ overview?.projectCount ?? 0 }}</strong>
              <p>清晰隔离项目上下文</p>
            </div>
          </article>
          <article class="stagger-5">
            <span class="metric-icon amber">⌕</span>
            <div>
              <small>检索模式</small
              ><strong class="mode-text">{{
                overview?.searchMode === "HYBRID" ? "混合" : "关键词"
              }}</strong>
              <p>模型不可用时自动降级</p>
            </div>
          </article>
          <article class="stagger-6">
            <span class="metric-icon mint">◷</span>
            <div>
              <small>待确认 / MCP</small
              ><strong>{{ overview?.candidateCount ?? 0 }}</strong>
              <p>
                {{ overview?.mcpStatus === "READY" ? "MCP 已就绪" : "MCP 未就绪" }}
                · {{ overview?.recentAssistantName ?? "暂无 AI 调用" }}
              </p>
            </div>
          </article>
        </section>
        <section class="content-grid">
          <article class="content-card stagger-5">
            <div class="section-heading">
              <div>
                <small>RECENT MEMORY</small>
                <h3>最近记忆</h3>
              </div>
              <button type="button" @click="selectNavigation('memories')">
                查看全部 →
              </button>
            </div>
            <div v-if="overview?.recentMemories.length" class="recent-list">
              <button
                v-for="memory in overview.recentMemories"
                :key="memory.id"
                type="button"
                @click="selectMemory(memory)"
              >
                <span>✦</span>
                <div>
                  <strong>{{ memory.title }}</strong
                  ><small
                    >{{ memory.projectName ?? "个人记忆" }} ·
                    {{ new Date(memory.updatedAt).toLocaleDateString() }}</small
                  >
                </div>
              </button>
            </div>
            <div v-else class="empty-content">
              <span>✦</span>
              <h4>还没有长期记忆</h4>
              <p>在右侧快速记录写下第一条偏好、决策或经验。</p>
            </div>
          </article>
          <article class="content-card stagger-6">
            <div class="section-heading">
              <div>
                <small>ACTIVE PROJECTS</small>
                <h3>活跃项目</h3>
              </div>
              <button type="button" @click="openNewProject">新建项目 ＋</button>
            </div>
            <div v-if="overview?.activeProjects.length" class="project-list">
              <button
                v-for="project in overview.activeProjects"
                :key="project.id"
                type="button"
                @click="
                  selectMemoryView('PROJECT', project.id);
                  selectNavigation('memories');
                "
              >
                <i :style="{ background: project.color }"></i>
                <div>
                  <strong>{{ project.name }}</strong
                  ><small>{{
                    project.workspaceBound
                      ? `标识 · ${project.workspaceIdentifier}`
                      : project.description || "尚未绑定工作空间"
                  }}</small>
                </div>
              </button>
            </div>
            <div v-else class="empty-content compact-empty">
              <span>▦</span>
              <h4>还没有项目</h4>
              <p>创建项目后可隔离不同工作的长期记忆。</p>
            </div>
          </article>
        </section>
        <section v-if="GRAPH_FEATURE_ENABLED" class="graph-preview stagger-7">
          <div>
            <small>MEMORY GRAPH</small>
            <h3>记忆图谱预览</h3>
            <p>
              {{
                overviewGraph && overviewGraph.nodes.length
                  ? `最近 ${overviewGraph.nodes.length} 条记忆的真实关系云`
                  : "记忆之间会基于语义与关键词自动建立联系"
              }}
            </p>
            <button type="button" @click="selectNavigation('graph')">
              展开图谱 →
            </button>
          </div>
          <button
            type="button"
            class="preview-canvas-wrap"
            aria-label="打开完整记忆图谱"
            @click="selectNavigation('graph')"
          >
            <canvas ref="overviewGraphCanvas"></canvas>
            <span v-if="!overviewGraph || overviewGraph.nodes.length === 0" class="preview-empty">
              创建更多记忆后，这里会呈现圆形关系云
            </span>
            <div class="graph-project-legend static-legend">
              <span><i :style="{ background: isDarkTheme ? '#a48ade' : '#7c5cb8' }"></i>个人记忆</span>
              <span
                v-for="project in activeProjects.slice(0, 2)"
                :key="project.id"
              >
                <i :style="{ background: project.color }"></i>{{ project.name }}
              </span>
            </div>
          </button>
        </section>
      </div>

      <section
        v-else-if="activeNavigation === 'memories'"
        class="memories-page page-enter"
      >
        <aside class="filter-panel">
          <div class="filter-title">
            <strong>记忆库</strong
            ><button
              type="button"
              aria-label="新建项目"
              @click="openNewProject"
            >
              ＋
            </button>
          </div>
          <section class="memory-filter-group">
            <small>记忆分类</small
            ><button
              :class="{ active: memoryView === 'ALL' }"
              type="button"
              @click="selectMemoryView('ALL', '')"
            >
              <span>◫</span>全部记忆<em>{{ memoryFacets.allCount }}</em></button
            ><button
              :class="{ active: memoryView === 'PERSONAL' }"
              type="button"
              @click="selectMemoryView('PERSONAL', '')"
            >
              <span>◇</span>个人记忆<em>{{
                memoryFacets.personalCount
              }}</em></button
            ><button
              :class="{ active: memoryView === 'PROJECT_ALL' }"
              type="button"
              @click="selectMemoryView('PROJECT_ALL', '')"
            >
              <span>▦</span>项目记忆<em>{{ memoryFacets.projectCount }}</em>
            </button>
          </section>
          <section class="memory-filter-group">
            <small>快捷访问</small
            ><button
              :class="{ active: memoryView === 'PENDING' }"
              type="button"
              @click="selectMemoryView('PENDING', '')"
            >
              <span>◷</span>待确认<em>{{ overview?.candidateCount ?? 0 }}</em></button
            ><button
              :class="{ active: memoryView === 'PINNED' }"
              type="button"
              @click="selectMemoryView('PINNED', '')"
            >
              <span>⌃</span>置顶<em>{{ memoryFacets.pinnedCount }}</em></button
            ><button
              :class="{ active: memoryView === 'FAVORITE' }"
              type="button"
              @click="selectMemoryView('FAVORITE', '')"
            >
              <span>☆</span>收藏<em>{{
                memoryFacets.favoriteCount
              }}</em></button
            ><button
              :class="{ active: memoryView === 'ARCHIVED' }"
              type="button"
              @click="selectMemoryView('ARCHIVED', '')"
            >
              <span>□</span>已归档<em>{{ memoryFacets.archivedCount }}</em>
            </button>
          </section>
          <section class="memory-filter-group project-space-group">
            <div class="project-space-header">
              <small>项目空间</small
              ><button
                type="button"
                aria-label="新建项目"
                @click="openNewProject"
              >
                ＋
              </button>
            </div>
            <!-- 项目多时区域内滚动，避免把记忆页面拉长 -->
            <div class="project-list-scroll">
              <div
                v-for="project in activeProjects"
                :key="project.id"
                class="project-filter-row"
              >
                <button
                  :class="{
                    active:
                      memoryView === 'PROJECT' &&
                      selectedProjectId === project.id,
                  }"
                  type="button"
                  @click="selectMemoryView('PROJECT', project.id)"
                >
                  <i :style="{ background: project.color }"></i
                  ><span
                    ><strong>{{ project.name }}</strong
                    ><small>{{
                      project.workspaceBound
                        ? `标识 · ${project.workspaceIdentifier}`
                        : "尚未绑定工作空间"
                    }}</small></span
                  ><em>{{
                    memoryFacets.projectCounts[project.id] ?? 0
                  }}</em></button
                ><button
                  class="project-edit-button"
                  type="button"
                  :aria-label="`编辑${project.name}`"
                  @click="openProjectEditor(project)"
                >
                  ···
                </button>
              </div>
            </div>
          </section>
        </aside>
        <div class="memory-list-panel">
          <header class="memory-page-heading">
            <div>
              <small>MEMORY LIBRARY</small>
              <h2>{{ memoryViewTitle }}</h2>
              <p>{{ memoryViewDescription }}</p>
            </div>
            <span>{{
              memoryView === "PENDING"
                ? filteredCandidates.length
                : memoryView === "ARCHIVED" && archivedTab === "projects"
                  ? archivedProjects.length
                  : filteredMemories.length
            }} 条当前结果</span>
          </header>
          <div
            v-if="memoryView !== 'PENDING' && !(memoryView === 'ARCHIVED' && archivedTab === 'projects')"
            class="memory-toolbar"
          >
            <label class="list-search"
              ><span>⌕</span
              ><input
                v-model="searchQuery"
                type="search"
                :placeholder="`在${memoryViewTitle}中搜索`" /></label
            ><select v-model="listType" aria-label="记忆类型">
              <option value="">全部类型</option>
              <option value="NOTE">笔记</option>
              <option value="PREFERENCE">偏好</option>
              <option value="DECISION">决策</option>
              <option value="SOLUTION">方案</option></select
            ><input
              v-model="listTag"
              class="tag-filter"
              type="search"
              placeholder="标签筛选"
              aria-label="标签筛选"
            /><select v-model.number="listImportance" aria-label="最低重要度">
              <option :value="1">全部重要度</option>
              <option :value="3">重要度 ≥ 3</option>
              <option :value="4">重要度 ≥ 4</option>
              <option :value="5">重要度 5</option>
            </select>
          </div>
          <div v-if="memoryView === 'PENDING'" class="candidate-list">
            <div class="memory-toolbar candidate-toolbar">
              <select v-model="candidateScopeFilter" aria-label="候选范围">
                <option value="">个人与项目</option>
                <option value="Personal">个人</option>
                <option value="Project">项目</option>
              </select>
              <select v-model="selectedProjectId" aria-label="候选项目">
                <option value="">全部项目</option>
                <option
                  v-for="project in activeProjects"
                  :key="project.id"
                  :value="project.id"
                >
                  {{ project.name }}
                </option>
              </select>
              <select v-model="candidateSourceFilter" aria-label="候选来源">
                <option value="">全部来源</option>
                <option
                  v-for="source in candidateSources"
                  :key="source"
                  :value="source"
                >
                  {{ source }}
                </option>
              </select>
            </div>
            <article v-for="candidate in filteredCandidates" :key="candidate.id">
              <div>
                <small>{{ candidate.sourceName }} · {{ candidate.projectName ?? "个人" }}</small>
                <h3>{{ candidate.title }}</h3>
                <p>{{ candidate.summary || candidate.content }}</p>
              </div>
              <div class="candidate-actions">
                <button class="secondary-button" type="button" @click="openCandidateEditor(candidate)">编辑</button>
                <button class="danger-button" type="button" @click="rejectCandidate(candidate)">拒绝</button>
                <button class="primary-button" type="button" @click="confirmCandidate(candidate)">确认</button>
              </div>
            </article>
            <div v-if="!filteredCandidates.length" class="empty-page">
              <span>◷</span><h2>没有待确认候选</h2><p>AI 提交的候选会先在这里等待确认。</p>
            </div>
          </div>
          <div v-else-if="isLoading" class="loading-state">正在读取本地记忆…</div>
          <template v-else>
            <!-- 已归档视图：双标签（已归档记忆 | 已归档项目） -->
            <div v-if="memoryView === 'ARCHIVED'" class="archived-tabs">
              <button
                type="button"
                :class="{ active: archivedTab === 'memories' }"
                @click="archivedTab = 'memories'"
              >
                <span>已归档记忆</span><em>{{ memoryFacets.archivedCount }}</em>
              </button>
              <button
                type="button"
                :class="{ active: archivedTab === 'projects' }"
                @click="archivedTab = 'projects'"
              >
                <span>已归档项目</span><em>{{ archivedProjects.length }}</em>
              </button>
            </div>

            <!-- 已归档项目标签：卡片列表，点击展开项目内记忆（手风琴） -->
            <div
              v-if="memoryView === 'ARCHIVED' && archivedTab === 'projects'"
              class="archived-project-list"
            >
              <article
                v-for="project in archivedProjects"
                :key="project.id"
                class="archived-project-card"
                :class="{ expanded: expandedArchivedProjectId === project.id }"
              >
                <header
                  class="archived-project-card-head"
                  role="button"
                  tabindex="0"
                  @click="toggleArchivedProject(project.id)"
                  @keydown.enter="toggleArchivedProject(project.id)"
                >
                  <i :style="{ background: project.color }"></i>
                  <div class="archived-project-card-title">
                    <strong>{{ project.name }}</strong>
                    <small
                      >{{ project.totalMemoryCount }} 条记忆 · 点击{{
                        expandedArchivedProjectId === project.id ? "收起" : "查看"
                      }}</small
                    >
                  </div>
                  <span class="archived-project-caret">{{
                    expandedArchivedProjectId === project.id ? "▾" : "▸"
                  }}</span>
                </header>
                <div class="archived-project-card-actions">
                  <button type="button" @click="restoreProject(project)">
                    恢复
                  </button>
                  <button
                    type="button"
                    class="danger"
                    @click="deleteProjectPermanently(project)"
                  >
                    彻底删除
                  </button>
                </div>
                <div
                  v-if="expandedArchivedProjectId === project.id"
                  class="archived-project-memories"
                >
                  <template v-if="archivedProjectMemories(project.id).length">
                    <article
                      v-for="memory in archivedProjectMemories(project.id)"
                      :key="memory.id"
                      :class="[
                        { selected: selectedMemory?.id === memory.id },
                        getMemoryTypeClass(memory.memoryType),
                      ]"
                      @click="selectMemory(memory)"
                    >
                      <div class="memory-card-details">
                        <span class="memory-type-chip">{{
                          getMemoryTypeLabel(memory.memoryType)
                        }}</span
                        ><span>重要度 {{ memory.importance }}</span>
                      </div>
                      <h3>{{ memory.title }}</h3>
                      <p>{{ memory.summary || memory.content }}</p>
                      <div class="memory-card-footer">
                        <div v-if="memory.tags.length" class="tag-row">
                          <span v-for="tag in memory.tags.slice(0, 4)" :key="tag"
                            ># {{ tag }}</span
                          >
                        </div>
                        <span class="memory-source">{{
                          memory.createdSource
                        }}</span>
                      </div>
                    </article>
                  </template>
                  <p v-else class="archived-project-empty">
                    该项目下没有已归档记忆
                  </p>
                </div>
              </article>
              <div v-if="!archivedProjects.length" class="empty-page">
                <span>✦</span>
                <h2>没有已归档项目</h2>
                <p>在项目空间归档项目后，会在这里统一管理。</p>
              </div>
            </div>

            <!-- 已归档记忆标签 / 其他视图：记忆列表 -->
            <div v-if="memoryView !== 'ARCHIVED' || archivedTab === 'memories'">
              <div
                v-if="filteredMemories.length"
                :class="memoryView === 'ARCHIVED' ? 'archived-groups' : 'memory-list'"
              >
                <!-- 已归档视图按「个人 / 项目」分组展示（分组竖排，组内卡片多列） -->
                <template v-if="memoryView === 'ARCHIVED'">
                  <section
                    v-for="group in archivedGroups"
                    :key="group.key"
                    class="archived-group"
                  >
                    <header class="archived-group-head">
                      <i
                        v-if="group.color"
                        :style="{ background: group.color }"
                      ></i>
                      <small>{{ group.title }}</small>
                      <em>{{ group.items.length }}</em>
                    </header>
                    <div class="memory-list archived-group-list">
                      <article
                        v-for="memory in group.items"
                        :key="memory.id"
                        :class="[
                          { selected: selectedMemory?.id === memory.id },
                          getMemoryTypeClass(memory.memoryType),
                        ]"
                        @click="selectMemory(memory)"
                      >
                        <div class="memory-meta">
                          <span>{{ memory.projectName ?? "个人" }}</span>
                          <div
                            v-if="memory.isFavorite || memory.isPinned"
                            class="memory-card-markers"
                          >
                            <span
                              v-if="memory.isFavorite"
                              class="favorite-marker"
                              title="已收藏"
                              >收藏</span
                            ><span
                              v-if="memory.isPinned"
                              class="pinned-marker"
                              title="已置顶"
                              >置顶</span
                            >
                          </div>
                        </div>
                        <div class="memory-card-details">
                          <span class="memory-type-chip">{{
                            getMemoryTypeLabel(memory.memoryType)
                          }}</span
                          ><span>重要度 {{ memory.importance }}</span>
                        </div>
                        <h3>{{ memory.title }}</h3>
                        <p>{{ memory.summary || memory.content }}</p>
                        <div class="memory-card-footer">
                          <div v-if="memory.tags.length" class="tag-row">
                            <span
                              v-for="tag in memory.tags.slice(0, 4)"
                              :key="tag"
                              ># {{ tag }}</span
                            >
                          </div>
                          <span class="memory-source">{{
                            memory.createdSource
                          }}</span>
                        </div>
                      </article>
                    </div>
                  </section>
                </template>
                <template v-else>
                  <article
                    v-for="memory in filteredMemories"
                    :key="memory.id"
                    :class="[
                      { selected: selectedMemory?.id === memory.id },
                      getMemoryTypeClass(memory.memoryType),
                    ]"
                    @click="selectMemory(memory)"
                  >
                    <div class="memory-meta">
                      <span>{{ memory.projectName ?? "个人" }}</span>
                      <!-- 搜索结果视图：右侧显示匹配度（与收藏/置顶位置冲突，搜索时不显示标记） -->
                      <div v-if="isSearchResultView" class="memory-card-markers">
                        <span class="match-score" title="语义匹配度">{{
                          matchLabel(memory.id)
                        }}</span>
                      </div>
                      <div
                        v-else-if="memory.isFavorite || memory.isPinned"
                        class="memory-card-markers"
                      >
                        <span
                          v-if="memory.isFavorite"
                          class="favorite-marker"
                          title="已收藏"
                          >收藏</span
                        ><span
                          v-if="memory.isPinned"
                          class="pinned-marker"
                          title="已置顶"
                          >置顶</span
                        >
                      </div>
                    </div>
                    <div class="memory-card-details">
                      <span class="memory-type-chip">{{
                        getMemoryTypeLabel(memory.memoryType)
                      }}</span
                      ><span>重要度 {{ memory.importance }}</span>
                    </div>
                    <h3>{{ memory.title }}</h3>
                    <p>{{ memory.summary || memory.content }}</p>
                    <div class="memory-card-footer">
                      <div v-if="memory.tags.length" class="tag-row">
                        <span v-for="tag in memory.tags.slice(0, 4)" :key="tag"
                          ># {{ tag }}</span
                        >
                      </div>
                      <span class="memory-source">{{
                        memory.createdSource
                      }}</span>
                    </div>
                  </article>
                </template>
                <button
                  v-if="nextCursor && !searchQuery.trim()"
                  class="load-more"
                  type="button"
                  @click="loadMemories(true)"
                >
                  加载更多
                </button>
              </div>
              <div v-else class="empty-page">
                <span>✦</span>
                <h2>{{ memoryEmptyTitle }}</h2>
                <p>{{ memoryEmptyHint }}</p>
                <button
                  v-if="memoryView !== 'ARCHIVED' && !isSearchActive"
                  class="primary-button"
                  type="button"
                  @click="openNewMemory"
                >
                  创建记忆
                </button>
              </div>
            </div>
          </template>
        </div>
      </section>

      <ProjectDocumentsPage
        v-else-if="activeNavigation === 'documents'"
        :projects="projects"
      />

      <section
        v-else-if="activeNavigation === 'connections'"
        class="connections-page page-enter"
      >
        <header class="connections-heading">
          <div>
            <small>LOCAL MCP · stdio</small>
            <h2>连接你的 AI</h2>
            <p>
              每个 AI 工具使用独立 Token，可单独编辑、轮换或吊销。Token
              默认雾化，悬停或键盘聚焦时显示，离开页面或按 Esc 后立即清除前端明文。
            </p>
          </div>
          <div
            class="connection-ready"
            :class="{ failed: mcpConnection?.status !== 'READY' }"
          >
            <i></i
            >{{
              mcpConnection?.status === "READY"
                ? "MCP 本地服务已就绪"
                : (mcpConnection?.errorMessage ?? "正在读取连接信息")
            }}
          </div>
        </header>
        <div class="section-title-row">
          <div>
            <small>CONNECTED AI</small>
            <h3>已接入的 AI</h3>
          </div>
          <span>{{ mcpClients.length }} 个</span>
        </div>
        <div class="mcp-client-grid">
          <article
            v-for="client in mcpClients"
            :key="client.sessionId"
            class="mcp-client-card"
          >
            <header class="mcp-client-head">
              <span
                class="mcp-client-logo"
                :style="{ background: getLogoColor(client.displayName) }"
                >{{ getLogoLetter(client.displayName) }}</span
              >
              <div class="mcp-client-title">
                <h3>{{ getDisplayTitle(client.displayName) }}</h3>
                <small>{{ mcpClientScopeText(client) }}</small>
              </div>
              <span
                class="mcp-client-status"
                :class="mcpClientStatusClass(client)"
                >{{ mcpClientStatusText(client) }}</span
              >
            </header>
            <p class="mcp-client-meta">
              <span>{{
                client.permission === "ReadWrite" ? "读写" : "只读"
              }}</span
              ><span>{{ mcpClientExpiresText(client) }}</span
              ><span>{{ mcpClientUsageText(client) }}</span>
            </p>
            <div
              v-if="mcpPathHealth[client.sessionId]?.supported && mcpPathHealth[client.sessionId]?.configured && !mcpPathHealth[client.sessionId]?.healthy"
              class="mcp-path-warning"
            >
              <small
                >程序位置已变化，该客户端配置已失效（配置仍指向
                {{ mcpPathHealth[client.sessionId]?.registeredCommand }}）</small
              >
              <button
                type="button"
                class="primary inline"
                :disabled="isMcpRegistering"
                @click="reRegisterMcpClient(client)"
              >
                {{ isMcpRegistering ? "重新注册中…" : "一键重新注册" }}
              </button>
            </div>
            <div
              v-else-if="mcpPathHealth[client.sessionId]?.usesLegacyEnvToken"
              class="mcp-path-warning legacy"
            >
              <small
                >该客户端仍在使用环境变量令牌接入，将在 0.5.0 版本移除，建议改用会话
                ID 接入</small
              >
              <button
                type="button"
                :disabled="isMcpRegistering"
                @click="reRegisterMcpClient(client)"
              >
                {{ isMcpRegistering ? "重新注册中…" : "改用会话 ID" }}
              </button>
            </div>
            <footer class="mcp-client-foot">
              <button
                type="button"
                @click="openEditMcpClientDialog(client, 'issue')"
              >
                编辑
              </button>
              <button
                type="button"
                @click="openEditMcpClientDialog(client, 'config')"
              >
                查看配置
              </button>
              <button type="button" @click="openPromptDialog(client)">
                查看提示词
              </button>
            </footer>
          </article>
          <article
            class="mcp-client-card add-card"
            role="button"
            tabindex="0"
            @click="openNewMcpClientDialog"
            @keydown.enter="openNewMcpClientDialog"
          >
            <span class="plus-icon">＋</span>
            <h3>连接你的 AI</h3>
            <p>签发独立令牌 · 本地 stdio 接入</p>
          </article>
        </div>
      </section>


      <section
        v-else-if="activeNavigation === 'settings'"
        class="settings-page page-enter"
      >
        <article class="settings-card">
          <div>
            <small>SEMANTIC SEARCH</small>
            <h2>Embedding 模型</h2>
            <p>
              可选配置 OpenAI 兼容
              API。未配置或调用失败时，系统继续使用本地关键词检索。
            </p>
          </div>
          <form @submit.prevent="saveEmbeddingSettings">
            <label
              >API 地址<input
                v-model="embedding.baseUrl"
                type="url"
                required /></label
            ><label
              >模型名称<input v-model="embedding.model" type="text" required
            /></label>
            <div class="form-row">
              <label
                >向量维度<input
                  v-model.number="embedding.dimensions"
                  type="number"
                  min="128"
                  max="1536"
                  required /></label
              ><label class="embedding-key-label"
                >API Key<!-- 已配置：明文雾化展示（悬停显形，超长截断）；单击复制，双击编辑 -->
                <code
                  v-if="embedding.configured && !isEmbeddingKeyEditing"
                  class="mcp-secret-code embedding-key-display"
                  title="悬停查看 · 单击复制 · 双击编辑"
                  role="button"
                  tabindex="0"
                  @click="onApiKeyClick"
                  @dblclick="onApiKeyDblclick"
                  @keydown.enter="onApiKeyDblclick"
                  >{{ embedding.apiKey }}</code
                >
                <input
                  v-else
                  v-model="embeddingApiKey"
                  type="text"
                  placeholder="输入后将使用 DPAPI 加密存储"
                  required
                  @keydown.esc="cancelEmbeddingKeyEdit"
              /></label>
            </div>
            <label class="check-label"
              ><input
                v-model="embedding.enabled"
                type="checkbox"
              />启用语义混合检索</label
            >
            <div class="form-actions">
              <button
                class="secondary-button"
                type="button"
                :disabled="!embedding.configured"
                @click="rebuildEmbeddings"
              >
                重新生成全部向量</button
              ><button
                class="primary-button"
                type="submit"
                :disabled="isSaving"
              >
                {{ isSaving ? "正在测试…" : "测试并保存" }}
              </button>
            </div>
          </form>
        </article>
      </section>

      <GraphPage
        v-else-if="activeNavigation === 'graph'"
        :projects="projects"
        :is-dark="isDarkTheme"
        :initial-memory-id="graphEntryMemoryId"
        @open-memory="openMemoryFromGraph"
        @open-search="openGraphSearch"
      />

      <section v-else class="placeholder-page page-enter">
        <span>⌁</span>
        <h2>{{ pageTitle }}</h2>
        <p>当前页面不可用，请返回总览后重试。</p>
        <button type="button" @click="selectNavigation('overview')">
          返回总览
        </button>
      </section>
    </section>

    <aside
      v-if="selectedMemory"
      class="detail-drawer"
      :class="getMemoryTypeClass(selectedMemory.memoryType)"
      aria-label="记忆详情"
      @click.stop
    >
      <button
        class="drawer-close"
        type="button"
        aria-label="关闭记忆详情"
        @click="closeMemoryDetails"
      >
        ×
      </button>
      <header class="drawer-header">
        <div class="drawer-eyebrow">
          <span class="scope-badge">{{
            selectedMemory.projectName ?? "个人记忆"
          }}</span
          ><span class="type-badge">{{
            getMemoryTypeLabel(selectedMemory.memoryType)
          }}</span
          ><span
            v-if="selectedMemory.status === 'Archived'"
            class="archived-badge"
            >已归档</span
          >
        </div>
        <h2>{{ selectedMemory.title }}</h2>
        <p v-if="selectedMemory.summary" class="memory-summary">
          {{ selectedMemory.summary }}
        </p>
        <div v-if="selectedMemory.tags.length" class="drawer-tags drawer-tags-top">
          <span v-for="tag in selectedMemory.tags" :key="tag"># {{ tag }}</span>
        </div>
        <div class="drawer-meta-row">
          <div class="drawer-metadata">
            <span>重要度 {{ selectedMemory.importance }}</span
            ><span
              >更新于
              {{ new Date(selectedMemory.updatedAt).toLocaleString() }}</span
            ><span>{{
              selectedMemory.cloudProcessingAllowed
                ? "允许语义检索"
                : "仅本地保存"
            }}</span>
          </div>
          <div
            v-if="selectedMemory.status === 'Active'"
            class="drawer-header-actions"
            aria-label="记忆状态"
          >
            <button
              type="button"
              :class="{ active: selectedMemory.isFavorite }"
              @click="
                updateMemoryMarkers(
                  selectedMemory,
                  !selectedMemory.isFavorite,
                  selectedMemory.isPinned,
                )
              "
            >
              {{ selectedMemory.isFavorite ? "已收藏" : "收藏" }}</button
            ><button
              type="button"
              :class="{ active: selectedMemory.isPinned }"
              @click="
                updateMemoryMarkers(
                  selectedMemory,
                  selectedMemory.isFavorite,
                  !selectedMemory.isPinned,
                )
              "
            >
              {{ selectedMemory.isPinned ? "已置顶" : "置顶" }}
            </button>
          </div>
        </div>
      </header>
      <div class="drawer-body">
        <section class="memory-content-card">
          <small>记忆内容 · Markdown</small>
          <div
            class="memory-content markdown-content"
            v-html="renderMemoryMarkdown(selectedMemory.content)"
          ></div>
        </section>
      </div>
      <footer class="drawer-actions">
        <button
          v-if="GRAPH_FEATURE_ENABLED"
          class="secondary-button graph-entry-button"
          type="button"
          @click="openGraphFromMemory(selectedMemory)"
        >
          在图谱中查看
        </button>
        <template v-if="selectedMemory.status === 'Archived'"
          ><button
            class="secondary-button"
            type="button"
            @click="restoreArchivedMemory(selectedMemory)"
          >
            恢复记忆</button
          ><button
            class="danger-button"
            type="button"
            @click="openDeleteConfirmation(selectedMemory)"
          >
            彻底删除
          </button></template
        ><template v-else
          ><button
            class="primary-button"
            type="button"
            @click="openMemoryEditor(selectedMemory)"
          >
            编辑记忆</button
          ><button
            class="secondary-button archive-button"
            type="button"
            @click="openArchiveConfirmation(selectedMemory)"
          >
            移至归档
          </button></template
        >
      </footer>
    </aside>

    <div
      v-if="archiveConfirmationMemory"
      class="modal-backdrop confirmation-backdrop"
      @click.self="closeArchiveConfirmation"
    >
      <section
        class="confirmation-card"
        role="dialog"
        aria-modal="true"
        aria-labelledby="archive-confirmation-title"
      >
        <button
          class="confirmation-close"
          type="button"
          aria-label="关闭归档确认"
          :disabled="isConfirmingMemoryAction"
          @click="closeArchiveConfirmation"
        >
          ×</button
        ><span class="confirmation-icon archive-icon">□</span
        ><small>归档记忆</small>
        <h2 id="archive-confirmation-title">确定移至归档吗？</h2>
        <p>
          “{{
            archiveConfirmationMemory.title
          }}”将从当前记忆列表移除，之后仍可在“已归档”中恢复。
        </p>
        <div class="confirmation-actions">
          <button
            class="secondary-button"
            type="button"
            :disabled="isConfirmingMemoryAction"
            @click="closeArchiveConfirmation"
          >
            暂不归档</button
          ><button
            class="primary-button"
            type="button"
            :disabled="isConfirmingMemoryAction"
            @click="confirmArchiveMemory"
          >
            {{ isConfirmingMemoryAction ? "正在归档…" : "确认归档" }}
          </button>
        </div>
      </section>
    </div>

    <!-- 项目归档/彻底删除确认弹框（居中显示） -->
    <div
      v-if="projectConfirmation"
      class="modal-backdrop confirmation-backdrop"
      @click.self="closeProjectConfirmation"
    >
      <section
        class="confirmation-card"
        :class="{ danger: projectConfirmation.kind === 'delete' }"
        role="dialog"
        aria-modal="true"
        aria-labelledby="project-confirmation-title"
      >
        <button
          class="confirmation-close"
          type="button"
          aria-label="关闭项目确认"
          :disabled="isProjectActionRunning"
          @click="closeProjectConfirmation"
        >
          ×</button
        ><span
          class="confirmation-icon"
          :class="projectConfirmation.kind === 'delete' ? 'delete-icon' : 'archive-icon'"
          >{{
            projectConfirmation.kind === "delete" ? "✕" : "□"
          }}</span
        ><small>{{
          projectConfirmation.kind === "delete" ? "彻底删除项目" : "归档项目"
        }}</small>
        <h2 id="project-confirmation-title">
          {{
            projectConfirmation.kind === "delete"
              ? "彻底删除该项目吗？"
              : "确定归档该项目吗？"
          }}
        </h2>
        <p>
          <template v-if="projectConfirmation.kind === 'delete'">
            “{{ projectConfirmation.project.name }}”将被永久删除，无法恢复。<template
              v-if="projectConfirmation.memoryCount"
            >
              该项目下 {{ projectConfirmation.memoryCount }} 条记忆（含已归档）将一并永久删除。</template
            >
          </template>
          <template v-else>
            “{{ projectConfirmation.project.name }}”将移入「已归档」，之后可在其中恢复。<template
              v-if="projectConfirmation.memoryCount"
            >
              当前项目还有 {{ projectConfirmation.memoryCount }} 条记忆，归档后这些记忆将同步移入「已归档」。</template
            >
          </template>
        </p>
        <div class="confirmation-actions">
          <button
            class="secondary-button"
            type="button"
            :disabled="isProjectActionRunning"
            @click="closeProjectConfirmation"
          >
            取消</button
          ><button
            class="primary-button"
            type="button"
            :disabled="isProjectActionRunning"
            @click="confirmProjectAction"
          >
            {{
              isProjectActionRunning
                ? "处理中…"
                : projectConfirmation.kind === "delete"
                  ? "确认彻底删除"
                  : "确认归档"
            }}
          </button>
        </div>
      </section>
    </div>
    <div
      v-if="deleteConfirmationMemory"
      class="modal-backdrop confirmation-backdrop"
      @click.self="closeDeleteConfirmation"
    >
      <section
        class="confirmation-card danger-confirmation"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="delete-confirmation-title"
      >
        <button
          class="confirmation-close"
          type="button"
          aria-label="关闭删除确认"
          :disabled="isConfirmingMemoryAction"
          @click="closeDeleteConfirmation"
        >
          ×</button
        ><span class="confirmation-icon delete-icon">×</span
        ><small>永久删除</small>
        <h2 id="delete-confirmation-title">确定彻底删除吗？</h2>
        <p>
          “{{
            deleteConfirmationMemory.title
          }}”及其关联数据将被永久删除，此操作无法撤销。
        </p>
        <div class="confirmation-warning">
          删除后无法恢复，请确认这条记忆已不再需要。
        </div>
        <div class="confirmation-actions">
          <button
            class="secondary-button"
            type="button"
            :disabled="isConfirmingMemoryAction"
            @click="closeDeleteConfirmation"
          >
            取消</button
          ><button
            class="danger-solid-button"
            type="button"
            :disabled="isConfirmingMemoryAction"
            @click="confirmPermanentDelete"
          >
            {{ isConfirmingMemoryAction ? "正在删除…" : "彻底删除" }}
          </button>
        </div>
      </section>
    </div>
    <div
      v-if="showMemoryEditor"
      class="modal-backdrop"
      @click.self="closeMemoryEditor"
    >
      <section class="modal-card memory-editor">
        <div class="modal-heading">
          <div>
            <small>MEMORY EDITOR</small>
            <h2>{{ editingCandidateId ? "编辑候选" : (memoryForm.id ? "编辑记忆" : "新建记忆") }}</h2>
          </div>
          <button type="button" @click="closeMemoryEditor">×</button>
        </div>
        <div v-if="isLoadingCandidateStructure" class="loading-state">
          正在识别候选类型…
        </div>
        <ConclusionCardFields
          v-else-if="editingConclusionCandidate && editingCandidateId"
          :candidate-id="editingCandidateId"
          :expected-version="memoryForm.expectedVersion ?? 0"
          @updated="handleConclusionCandidateUpdated"
        />
        <form v-else class="memory-editor-form" @submit.prevent="saveMemory">
        <div class="form-row">
          <label
            >范围<select v-model="memoryForm.scope">
              <option value="Personal">个人记忆</option>
              <option value="Project">项目记忆</option>
            </select></label
          ><label v-if="memoryForm.scope === 'Project'"
            >项目<select v-model="memoryForm.projectId" required>
              <option value="" disabled>选择项目</option>
              <option
                v-for="project in activeProjects"
                :key="project.id"
                :value="project.id"
              >
                {{ project.name }}
              </option>
            </select></label
          ><label
            >类型<select v-model="memoryForm.memoryType">
              <option value="NOTE">笔记</option>
              <option value="PREFERENCE">偏好</option>
              <option value="DECISION">决策</option>
              <option value="SOLUTION">方案</option>
            </select></label
          >
        </div>
        <label
          >标题<input
            v-model="memoryForm.title"
            maxlength="200"
            required /></label
        ><label
          >摘要<textarea
            v-model="memoryForm.summary"
            rows="2"
          ></textarea></label
        ><label
          >正文<textarea
            v-model="memoryForm.content"
            rows="9"
            maxlength="200000"
            required
          ></textarea>
        </label>
        <div class="form-row">
          <label
            >关键词<input
              v-model="memoryForm.keywords"
              placeholder="使用逗号分隔" /></label
          ><label
            >标签<input
              v-model="memoryForm.tags"
              placeholder="使用逗号分隔" /></label
          ><label
            >重要度<input
              v-model.number="memoryForm.importance"
              type="number"
              min="1"
              max="5"
          /></label>
        </div>
        <label class="check-label"
          ><input
            v-model="memoryForm.cloudProcessingAllowed"
            type="checkbox"
          />允许发送给 Embedding API</label
        >
        <div class="form-actions">
          <button
            class="secondary-button"
            type="button"
            @click="closeMemoryEditor"
          >
            取消</button
          ><button class="primary-button" type="submit" :disabled="isSaving">
            {{ isSaving ? "保存中…" : "保存记忆" }}
          </button>
        </div>
        </form>
      </section>
    </div>
    <div
      v-if="showProjectEditor"
      class="modal-backdrop"
      @click.self="showProjectEditor = false"
    >
      <form class="modal-card" @submit.prevent="saveProject">
        <div class="modal-heading">
          <div>
            <small>PROJECT</small>
            <h2>{{ editingProjectId ? "编辑项目" : "新建项目" }}</h2>
          </div>
          <button type="button" @click="showProjectEditor = false">×</button>
        </div>
        <label
          >中文项目名称<input
            v-model="projectName"
            maxlength="80"
            required /></label
        ><label
          >项目说明<textarea
            v-model="projectDescription"
            rows="3"
          ></textarea></label
        ><label
          >工作空间标识（必填）<input
            v-model="projectWorkspaceIdentifier"
            maxlength="120"
            required
            placeholder="例如 mcp-ai-memory"
          /><small
            >必填；一般填写工作空间根目录名称；相同标识会自动归入当前中文项目。</small
          ></label
        ><label>标识颜色<input v-model="projectColor" type="color" /></label>
        <div class="form-actions">
          <button
            v-if="editingProjectId"
            class="danger-button"
            type="button"
            @click="archiveProject"
          >
            归档项目</button
          ><button
            class="secondary-button"
            type="button"
            @click="showProjectEditor = false"
          >
            取消</button
          ><button class="primary-button" type="submit">
            {{ editingProjectId ? "保存修改" : "创建项目" }}
          </button>
        </div>
      </form>
    </div>
    <div
      v-if="showMcpClientDialog"
      class="modal-backdrop mcp-dialog-backdrop"
      @click.self="closeMcpClientDialog"
    >
      <section
        class="modal-card mcp-config-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="mcp-dialog-title"
      >
        <header class="modal-heading">
          <div>
            <small>MCP CONNECTION</small>
            <h2 id="mcp-dialog-title">
              {{ editingMcpClient ? "编辑 AI 工具" : "接入你的 AI" }}
            </h2>
          </div>
          <button
            type="button"
            aria-label="关闭"
            @click="closeMcpClientDialog"
          >
            ×
          </button>
        </header>
        <p class="mcp-dialog-intro">
          每个 AI 工具对应一张卡片和一个令牌；轮换令牌后旧令牌立即失效，可随时重新生成。
        </p>
        <nav
          class="mcp-dialog-tabs"
          role="tablist"
          aria-label="MCP 接入方式"
        >
          <button
            type="button"
            role="tab"
            :aria-selected="mcpDialogTab === 'issue'"
            :class="{ active: mcpDialogTab === 'issue' }"
            @click="switchMcpDialogTab('issue')"
          >
            编辑 AI 工具
          </button>
          <button
            type="button"
            role="tab"
            :aria-selected="mcpDialogTab === 'config'"
            :class="{ active: mcpDialogTab === 'config' }"
            :disabled="!editingMcpClient"
            @click="switchMcpDialogTab('config')"
          >
            接入配置
          </button>
        </nav>

        <!-- 一、编辑 AI 工具 -->
        <section v-if="mcpDialogTab === 'issue'" class="mcp-dialog-section">
          <label class="mcp-form-label"
            >AI 工具名
            <input
              v-model="mcpClientDraft.displayName"
              class="mcp-form-input"
              type="text"
              maxlength="80"
              placeholder="例如 Codex、Claude Desktop、Cursor、WorkBuddy 或任意自定义名称"
              required
            />
          </label>
          <div class="mcp-form-row">
            <label class="mcp-form-label"
              >权限
              <select v-model="mcpClientDraft.permission" class="mcp-form-input">
                <option value="ReadWrite">读写：可保存和修改记忆</option>
                <option value="Read">只读：仅允许查询</option>
              </select>
            </label>
            <label class="mcp-form-label"
              >项目范围
              <select v-model="mcpClientDraft.projectId" class="mcp-form-input">
                <option value="">全部项目</option>
                <option
                  v-for="project in activeProjects"
                  :key="project.id"
                  :value="project.id"
                >
                  {{ project.name }}
                </option>
              </select>
            </label>
            <label class="mcp-form-label"
              >有效期
              <select
                v-model.number="mcpClientDraft.validityDays"
                class="mcp-form-input"
              >
                <option :value="0">长期有效</option>
                <option :value="7">7 天</option>
                <option :value="30">30 天</option>
                <option :value="90">90 天</option>
              </select>
            </label>
          </div>
          <div v-if="editingMcpClient" class="mcp-token-block">
            <div class="mcp-token-status-line">
              <span class="mcp-token-client">{{ editingMcpClient.displayName }}</span>
              <span
                class="mcp-token-status"
                :class="mcpClientStatusClass(editingMcpClient)"
                >{{ mcpClientStatusText(editingMcpClient) }}</span
              >
              <span class="mcp-token-status tone-amber">{{
                mcpClientExpiresText(editingMcpClient)
              }}</span>
              <span class="mcp-token-actions">
                <button
                  type="button"
                  class="primary inline"
                  :disabled="isMcpClientSaving || !mcpClientDraft.displayName.trim()"
                  @click="
                    editingMcpClient.tokenPrefix ? rotateMcpClientSessionId() : createMcpClient()
                  "
                >
                  {{
                    isMcpClientSaving
                      ? "处理中…"
                      : editingMcpClient.tokenPrefix
                        ? "轮换会话 ID"
                        : "生成令牌"
                  }}
                </button>
              </span>
            </div>
            <small class="mcp-config-tip">
              轮换会话 ID 会让旧会话 ID 立即失效（接入配置需重新写入）；名称、权限与令牌保持不变。
            </small>
          </div>
          <div class="mcp-form-actions">
            <button
              v-if="editingMcpClient"
              class="danger-button"
              type="button"
              :disabled="isMcpClientSaving"
              @click="revokeMcpClient"
            >
              删除 AI 工具
            </button>
            <button
              v-if="editingMcpClient"
              class="secondary-button"
              type="button"
              :disabled="isMcpClientSaving"
              @click="saveMcpClient"
            >
              {{ isMcpClientSaving ? "处理中…" : "保存修改" }}
            </button>
            <button
              v-if="!editingMcpClient || !editingMcpClient.tokenPrefix"
              class="primary-button"
              type="button"
              :disabled="isMcpClientSaving || !mcpClientDraft.displayName.trim()"
              @click="createMcpClient"
            >
              {{ isMcpClientSaving ? "处理中…" : "生成令牌" }}
            </button>
            <button
              class="secondary-button"
              type="button"
              @click="closeMcpClientDialog"
            >
              关闭
            </button>
          </div>
          <div v-if="issuedMcpClientToken" class="mcp-issued-token">
            <small>新令牌（仅展示一次，请立即复制并妥善保存）</small>
            <code>{{ issuedMcpClientToken }}</code>
            <button
              type="button"
              @click="copyMcpValue(issuedMcpClientToken, '令牌已复制')"
            >
              复制令牌
            </button>
          </div>
        </section>

        <!-- 二、接入配置 -->
        <section v-else class="mcp-dialog-section">
          <div v-if="editingMcpClient" class="mcp-config-block">
            <div class="mcp-config-title">
              <h3>stdio 接入配置（不监听端口）</h3>
              <div class="mcp-config-actions">
                <button
                  type="button"
                  :disabled="!activeMcpConfigText"
                  @click="copyMcpClientConfig"
                >
                  复制配置
                </button>
                <button
                  v-if="mcpConfigReport?.supported"
                  type="button"
                  class="primary inline"
                  :disabled="isMcpRegistering || !editingMcpClient.tokenPrefix"
                  @click="registerMcpClientConfig"
                >
                  {{ isMcpRegistering ? "写入中…" : "写入配置" }}
                </button>
              </div>
            </div>
            <p v-if="isMcpConfigLoading" class="mcp-config-tip">正在生成接入配置…</p>
            <template v-else-if="mcpConfigReport">
              <pre>{{ activeMcpConfigText }}</pre>
              <small class="mcp-config-tip">
                {{ mcpConfigReport.message }}
                <template v-if="mcpConfigReport.configPath"
                  >（{{ mcpConfigReport.configPath }}）</template
                >
                <template v-if="mcpConfigReport.backupPath"
                  >｜备份：{{ mcpConfigReport.backupPath }}</template
                >
              </small>
            </template>
            <p v-else class="mcp-config-tip">接入配置尚未加载，请重新切换标签或查看上方错误提示。</p>
          </div>
          <div v-if="editingMcpClient" class="mcp-config-block">
            <div class="mcp-config-title">
              <h3>会话 ID（stdio 接入标识）</h3>
              <button
                type="button"
                @click="copyMcpValue(editingMcpClient.sessionId, '会话 ID 已复制')"
              >
                复制会话 ID
              </button>
            </div>
            <code class="mcp-secret-code">{{ editingMcpClient.sessionId }}</code>
            <small class="mcp-config-tip">
              stdio 接入以会话 ID 标识身份（见上方 args），无需在客户端配置中填写令牌。
            </small>
          </div>
          <div v-if="editingMcpClient" class="mcp-form-actions">
            <button
              type="button"
              class="secondary-button"
              @click="testMcpClient(editingMcpClient)"
            >
              测试连接
            </button>
            <button
              class="secondary-button"
              type="button"
              @click="closeMcpClientDialog"
            >
              关闭
            </button>
          </div>
          <p v-else class="mcp-empty-hint">
            请先在「编辑 AI 工具」标签创建 AI 工具后再查看接入配置。
          </p>
        </section>
      </section>
    </div>
    <PromptDialog
      :visible="promptDialogVisible"
      :client-type="promptDialogClientType"
      :client-display-name="promptDialogClientName"
      @close="promptDialogVisible = false"
    />
    <Transition name="toast"
      ><div v-if="toastMessage" class="toast-message">
        ✓ {{ toastMessage }}
      </div></Transition
    >
  </main>
</template>
