<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'

import { apiFetch } from '../api'
import type {
  ProjectDocumentDraftItem,
  ProjectDocumentItem,
  ProjectDocumentOverview,
  ProjectDocumentType,
  ProjectItem,
} from '../api'
import { ApiRequestError } from '../api'
import { renderMemoryMarkdown } from '../markdown'
import { stripYamlHeader } from './projectDocumentText'
import { formatDateTimeInTimeZone } from './projectDocumentTime'

/**
 * 项目文档页：五份初始化草稿审核 / 晋升 / 正式文档只读 / 修复 / Embedding 设置。
 *
 * 布局遵循执行计划 §21.2/§21.3：左侧文件列表 + 右侧内容区；
 * 第五份批准后自动晋升（服务端 promote_drafts 兜底）。
 */

const props = defineProps<{
  projects: ProjectItem[]
}>()

const emit = defineEmits<{
  (event: 'refresh-projects'): void
}>()

const DOCUMENT_TYPES: { key: ProjectDocumentType; file: string; label: string }[] = [
  { key: 'CONTEXT', file: '01_CONTEXT.md', label: '项目背景' },
  { key: 'DECISIONS', file: '02_DECISIONS.md', label: '项目决策' },
  { key: 'CURRENT_STATUS', file: '03_CURRENT_STATUS.md', label: '当前状态' },
  { key: 'PROBLEMS', file: '04_PROBLEMS.md', label: '项目问题' },
  { key: 'CHANGELOG', file: '05_CHANGELOG.md', label: '变更记录' },
]

const STATUS_LABELS: Record<string, string> = {
  NOT_INITIALIZED: '未初始化',
  DRAFT_PENDING_REVIEW: '草稿待审核',
  DRAFT_PARTIALLY_APPROVED: '草稿部分批准',
  DRAFT_ALL_APPROVED: '草稿全部批准',
  PROMOTING: '晋升中 / 待恢复',
  ACTIVE: '正式文档已生效',
}

const SYNC_LABELS: Record<string, string> = {
  SYNCED: '已同步',
  SYNC_PENDING: '待同步',
  CONFLICT: '版本冲突',
  FORMAT_ERROR: '格式错误',
  REPAIR_PENDING: '待恢复',
}

const selectedProjectId = ref('')
const overview = ref<ProjectDocumentOverview | null>(null)
const selectedType = ref<ProjectDocumentType | null>(null)
const documentDetail = ref<ProjectDocumentItem | null>(null)
const draftEditingContent = ref('')
const isEditingDraft = ref(false)
const isLoading = ref(false)
const isWorking = ref(false)
const errorMessage = ref('')
const successMessage = ref('')
const showPrivacyNotice = ref(false)
/** 文档只读查看模式：默认渲染 Markdown，可切换查看原始语法。 */
const documentViewMode = ref<'rendered' | 'source'>('rendered')
/** 浏览器当前使用的 IANA 时区，用于展示服务端返回的绝对时间。 */
const currentTimeZone = Intl.DateTimeFormat().resolvedOptions().timeZone

const boundProjects = computed(() => props.projects.filter((project) => project.workspaceBound))

const isDraftMode = computed(() => {
  const status = overview.value?.status
  return status === 'DRAFT_PENDING_REVIEW' || status === 'DRAFT_PARTIALLY_APPROVED' || status === 'DRAFT_ALL_APPROVED'
})

const isFormalMode = computed(() => overview.value?.status === 'ACTIVE' || overview.value?.status === 'PROMOTING')

const selectedDraft = computed<ProjectDocumentDraftItem | null>(() => {
  if (!overview.value || !selectedType.value) return null
  return overview.value.drafts.find((draft) => draft.documentType === selectedType.value) ?? null
})

const selectedDocumentState = computed(() => {
  if (!overview.value || !selectedType.value) return null
  return overview.value.documents.find((doc) => doc.documentType === selectedType.value) ?? null
})

/** 剩余未批准数量（全部批准后提示自动晋升）。 */
const pendingCount = computed(
  () => overview.value?.drafts.filter((draft) => draft.reviewStatus !== 'APPROVED').length ?? 0,
)

/** 当前项目是否已有可浏览的草稿或正式文档。 */
const hasDocumentEntries = computed(() => {
  if (!overview.value) return false
  return overview.value.drafts.length > 0 || overview.value.documents.length > 0
})

/** 空状态标题。 */
const emptyStateTitle = computed(() => {
  if (isLoading.value) return '正在加载项目文档'
  return '尚未生成项目文档'
})

/** 空状态说明。 */
const emptyStateDescription = computed(() => {
  if (isLoading.value) return '正在读取项目文档状态，请稍候。'
  return '请先在该项目的 AI 会话中初始化项目文档，生成后可在这里逐份审核。'
})

watch(successMessage, (next) => {
  if (next.length === 0) return
  window.setTimeout(() => {
    if (successMessage.value === next) successMessage.value = ''
  }, 3000)
})

watch(errorMessage, (next) => {
  if (next.length === 0) return
  window.setTimeout(() => {
    if (errorMessage.value === next) errorMessage.value = ''
  }, 4000)
})

watch(selectedProjectId, (next) => {
  overview.value = null
  selectedType.value = null
  documentDetail.value = null
  isEditingDraft.value = false
  if (next) void refresh(next)
})

onMounted(() => {
  const first = boundProjects.value[0]
  if (first) selectedProjectId.value = first.id
})

async function refresh(projectId: string): Promise<void> {
  isLoading.value = true
  try {
    overview.value = await apiFetch<ProjectDocumentOverview>(
      `/api/projects/${projectId}/documents`,
      { method: 'GET' },
      null,
    )
    // 默认选中第一份（草稿或正式）。
    if (!selectedType.value) {
      if (isDraftMode.value && overview.value.drafts.length > 0) {
        selectedType.value = overview.value.drafts[0].documentType
      } else if (overview.value.documents.length > 0) {
        selectedType.value = overview.value.documents[0].documentType
      }
    }
    if (selectedType.value && isFormalMode.value) {
      await loadDocumentDetail(projectId, selectedType.value)
    } else {
      documentDetail.value = null
    }
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isLoading.value = false
  }
}

async function loadDocumentDetail(projectId: string, documentType: ProjectDocumentType): Promise<void> {
  try {
    documentDetail.value = await apiFetch<ProjectDocumentItem>(
      `/api/projects/${projectId}/documents/${documentType}`,
      { method: 'GET' },
      null,
    )
  } catch (error) {
    errorMessage.value = readError(error)
  }
}

function selectDocument(type: ProjectDocumentType): void {
  selectedType.value = type
  isEditingDraft.value = false
  const projectId = selectedProjectId.value
  if (projectId && isFormalMode.value) {
    void loadDocumentDetail(projectId, type)
  }
}

/** 打开草稿编辑（填入当前草稿正文，去掉 YAML 头）。 */
function startEditDraft(): void {
  const draft = selectedDraft.value
  if (!draft) return
  draftEditingContent.value = stripYamlHeader(draft.content)
  isEditingDraft.value = true
}

async function saveDraft(): Promise<void> {
  const draft = selectedDraft.value
  if (!draft || !isEditingDraft.value) return
  isWorking.value = true
  try {
    await apiFetch<ProjectDocumentDraftItem>(
      `/api/projects/${selectedProjectId.value}/documents/drafts/${draft.documentType}`,
      {
        method: 'PUT',
        body: JSON.stringify({ expectedVersion: draft.version, content: draftEditingContent.value }),
      },
      null,
    )
    isEditingDraft.value = false
    successMessage.value = '草稿已保存（审核状态回退为待审核）'
    await refresh(selectedProjectId.value)
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isWorking.value = false
  }
}

async function approveDraft(draft: ProjectDocumentDraftItem): Promise<void> {
  isWorking.value = true
  try {
    await apiFetch<ProjectDocumentDraftItem>(
      `/api/projects/${selectedProjectId.value}/documents/drafts/${draft.documentType}/approve`,
      { method: 'POST' },
      null,
    )
    successMessage.value = `「${labelOf(draft.documentType)}」已批准`
    await refresh(selectedProjectId.value)
    // 第五份批准后自动触发晋升（不再显示第二次总确认）。
    if (pendingCount.value === 0 && overview.value?.status === 'DRAFT_ALL_APPROVED') {
      await promote()
    }
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isWorking.value = false
  }
}

async function revokeDraft(draft: ProjectDocumentDraftItem): Promise<void> {
  isWorking.value = true
  try {
    await apiFetch<ProjectDocumentDraftItem>(
      `/api/projects/${selectedProjectId.value}/documents/drafts/${draft.documentType}/revoke`,
      { method: 'POST' },
      null,
    )
    await refresh(selectedProjectId.value)
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isWorking.value = false
  }
}

async function promote(): Promise<void> {
  isWorking.value = true
  try {
    await apiFetch<void>(
      `/api/projects/${selectedProjectId.value}/documents/drafts/promote`,
      { method: 'POST' },
      null,
    )
    successMessage.value = '五份正式文档已创建，初始化草稿已清理'
    await refresh(selectedProjectId.value)
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isWorking.value = false
  }
}

async function deleteDrafts(): Promise<void> {
  if (!window.confirm('确定删除全部初始化草稿？该操作不可恢复。')) return
  isWorking.value = true
  try {
    await apiFetch<void>(
      `/api/projects/${selectedProjectId.value}/documents/drafts`,
      { method: 'DELETE' },
      null,
    )
    successMessage.value = '初始化草稿已删除'
    await refresh(selectedProjectId.value)
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isWorking.value = false
  }
}

async function repairDocument(): Promise<void> {
  if (!selectedType.value) return
  isWorking.value = true
  try {
    await apiFetch<ProjectDocumentItem>(
      `/api/projects/${selectedProjectId.value}/documents/${selectedType.value}/repair`,
      { method: 'POST' },
      null,
    )
    successMessage.value = '文档已修复（正文保留，YAML 头已恢复）'
    await refresh(selectedProjectId.value)
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isWorking.value = false
  }
}

/** Embedding 开关：开启前必须显示隐私提示（§19.1）。 */
async function toggleEmbedding(): Promise<void> {
  if (!overview.value) return
  const next = !overview.value.embeddingEnabled
  if (next) {
    showPrivacyNotice.value = true
    return
  }
  isWorking.value = true
  try {
    await apiFetch<boolean>(
      `/api/projects/${selectedProjectId.value}/documents/embedding`,
      { method: 'PUT', body: JSON.stringify({ enabled: false }) },
      null,
    )
    successMessage.value = '项目文档云端嵌入已关闭'
    await refresh(selectedProjectId.value)
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isWorking.value = false
  }
}

async function confirmEnableEmbedding(): Promise<void> {
  showPrivacyNotice.value = false
  isWorking.value = true
  try {
    await apiFetch<boolean>(
      `/api/projects/${selectedProjectId.value}/documents/embedding`,
      { method: 'PUT', body: JSON.stringify({ enabled: true }) },
      null,
    )
    successMessage.value = '项目文档云端嵌入已开启（向量索引将按任务机制补建）'
    await refresh(selectedProjectId.value)
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isWorking.value = false
  }
}

function labelOf(type: ProjectDocumentType): string {
  return DOCUMENT_TYPES.find((entry) => entry.key === type)?.label ?? type
}

function fileOf(type: ProjectDocumentType): string {
  return DOCUMENT_TYPES.find((entry) => entry.key === type)?.file ?? `${type}.md`
}

/** 去掉 YAML 元数据后安全渲染项目文档 Markdown。 */
function renderProjectDocumentMarkdown(content: string): string {
  return renderMemoryMarkdown(stripYamlHeader(content))
}

function readError(error: unknown): string {
  if (error instanceof ApiRequestError) {
    if (error.code === 'PROJECT_DOCUMENT_WORKSPACE_UNBOUND') {
      return '该项目尚未记录工作空间路径：请先在该工作空间中通过 AI 调用项目文档工具'
    }
    if (error.code === 'PROJECT_DOCUMENT_VERSION_CONFLICT') {
      return '内容已被其他会话修改，请刷新后重试'
    }
    return error.message
  }
  return '请求失败，请稍后重试'
}

defineExpose({ refresh })
</script>

<template>
  <div class="documents-page page-enter">
    <header class="documents-header">
      <div class="documents-heading">
        <small>PROJECT KNOWLEDGE</small>
        <h2>文档工作区</h2>
        <p class="documents-subtitle">
          项目背景、决策、当前状态、问题与变更记录
        </p>
      </div>
      <div class="documents-actions">
        <label class="documents-project-picker">
          <span>当前项目</span>
          <select v-model="selectedProjectId" class="documents-project-select">
            <option value="" disabled>选择项目…</option>
            <option v-for="project in boundProjects" :key="project.id" :value="project.id">
              {{ project.name }}
            </option>
          </select>
        </label>
        <button
          class="documents-refresh-button"
          type="button"
          :disabled="!selectedProjectId || isLoading"
          aria-label="刷新项目文档"
          @click="selectedProjectId && refresh(selectedProjectId)"
        >
          <span :class="{ spinning: isLoading }" aria-hidden="true">↻</span>
          刷新
        </button>
      </div>
    </header>

    <section v-if="boundProjects.length === 0" class="documents-zero-state">
      <span class="documents-zero-icon" aria-hidden="true">◇</span>
      <h3>暂无可用项目</h3>
      <p>请先在项目工作空间中连接 MemStack。</p>
    </section>

    <template v-else>
      <section v-if="overview" class="documents-meta" aria-label="项目文档状态">
        <div class="documents-meta-summary">
          <span class="documents-status" :data-status="overview.status">
            {{ STATUS_LABELS[overview.status] ?? overview.status }}
          </span>
          <span
            v-if="overview.workspacePath"
            class="documents-workspace"
            :title="overview.workspacePath"
          >
            <b>最近工作空间</b><span>{{ overview.workspacePath }}</span>
          </span>
          <span v-if="overview.lastSyncedAt" class="documents-synced">
            最近同步 {{ formatDateTimeInTimeZone(overview.lastSyncedAt, currentTimeZone) }}
          </span>
        </div>
        <label class="documents-embedding">
          <span class="documents-embedding-copy">
            <strong>云端嵌入</strong>
            <small>{{ overview.embeddingEnabled ? '已开启' : '默认关闭' }}</small>
          </span>
          <input
            class="documents-embedding-input"
            type="checkbox"
            :checked="overview.embeddingEnabled"
            :disabled="isWorking || overview.status !== 'ACTIVE'"
            @change="toggleEmbedding"
          />
          <span class="documents-embedding-track" aria-hidden="true"><i></i></span>
        </label>
      </section>

      <div class="documents-body" :class="{ 'is-empty': !hasDocumentEntries }">
        <!-- 左侧：文件列表 -->
        <aside v-if="hasDocumentEntries" class="documents-list" aria-label="项目文档列表">
          <template v-if="isDraftMode && overview">
            <button
              v-for="draft in overview.drafts"
              :key="draft.documentType"
              type="button"
              class="documents-item"
              :class="{ active: selectedType === draft.documentType }"
              @click="selectDocument(draft.documentType)"
            >
              <span class="documents-item-name">{{ labelOf(draft.documentType) }}</span>
              <span class="documents-item-file">{{ draft.relativePath }}</span>
              <span class="documents-item-status" :class="{ approved: draft.reviewStatus === 'APPROVED' }">
                {{ draft.reviewStatus === 'APPROVED' ? `已批准 v${draft.version}` : `待审核 v${draft.version}` }}
              </span>
            </button>
            <div class="documents-list-footer">
              <p class="documents-hint">
                {{ pendingCount > 0 ? `剩余 ${pendingCount} 份待批准；第五份批准后自动创建正式文档` : '五份已全部批准' }}
              </p>
              <button class="ghost-button" type="button" :disabled="isWorking" @click="deleteDrafts">
                删除全部草稿
              </button>
            </div>
          </template>
          <template v-else-if="overview">
            <button
              v-for="doc in overview.documents"
              :key="doc.documentType"
              type="button"
              class="documents-item"
              :class="{ active: selectedType === doc.documentType }"
              @click="selectDocument(doc.documentType)"
            >
              <span class="documents-item-name">{{ labelOf(doc.documentType) }}</span>
              <span class="documents-item-file">{{ doc.relativePath }}</span>
              <span
                class="documents-item-status"
                :class="{ error: doc.syncStatus === 'FORMAT_ERROR' || doc.syncStatus === 'CONFLICT', missing: !doc.fileExists }"
              >
                {{ SYNC_LABELS[doc.syncStatus] ?? doc.syncStatus }} · v{{ doc.version }}
              </span>
            </button>
          </template>
        </aside>

        <!-- 右侧：内容区 -->
        <section class="documents-content">
          <!-- 草稿审核 -->
          <template v-if="isDraftMode && selectedDraft">
            <header class="documents-content-header">
              <div>
                <h3>{{ labelOf(selectedDraft.documentType) }} <small>{{ selectedDraft.relativePath }}</small></h3>
                <p v-if="selectedDraft.lastChangeReason" class="documents-reason">
                  最近变化：{{ selectedDraft.lastChangeReason }}
                </p>
              </div>
              <div v-if="!isEditingDraft" class="documents-content-actions">
                <div class="documents-view-switch" aria-label="文档查看模式">
                  <button
                    type="button"
                    :class="{ active: documentViewMode === 'rendered' }"
                    :aria-pressed="documentViewMode === 'rendered'"
                    @click="documentViewMode = 'rendered'"
                  >
                    预览
                  </button>
                  <button
                    type="button"
                    :class="{ active: documentViewMode === 'source' }"
                    :aria-pressed="documentViewMode === 'source'"
                    @click="documentViewMode = 'source'"
                  >
                    原文
                  </button>
                </div>
                <button class="ghost-button" type="button" :disabled="isWorking" @click="startEditDraft">编辑</button>
                <button
                  v-if="selectedDraft.reviewStatus !== 'APPROVED'"
                  class="primary-button compact"
                  type="button"
                  :disabled="isWorking"
                  @click="approveDraft(selectedDraft)"
                >
                  批准
                </button>
                <button
                  v-else
                  class="ghost-button"
                  type="button"
                  :disabled="isWorking"
                  @click="revokeDraft(selectedDraft)"
                >
                  撤销批准
                </button>
              </div>
              <div v-else class="documents-content-actions">
                <button class="ghost-button" type="button" :disabled="isWorking" @click="isEditingDraft = false">取消</button>
                <button class="primary-button compact" type="button" :disabled="isWorking" @click="saveDraft">保存</button>
              </div>
            </header>
            <div
              v-if="!isEditingDraft && documentViewMode === 'rendered'"
              class="documents-viewer documents-markdown markdown-content"
              v-html="renderProjectDocumentMarkdown(selectedDraft.content)"
            ></div>
            <pre v-else-if="!isEditingDraft" class="documents-viewer">{{ stripYamlHeader(selectedDraft.content) }}</pre>
            <textarea v-else v-model="draftEditingContent" class="documents-editor" spellcheck="false"></textarea>
          </template>

          <!-- 正式文档（只读） -->
          <template v-else-if="isFormalMode && selectedDocumentState">
            <header class="documents-content-header">
              <div>
                <h3>{{ labelOf(selectedDocumentState.documentType) }} <small>{{ selectedDocumentState.relativePath }}</small></h3>
                <p class="documents-reason">
                  版本 v{{ selectedDocumentState.version }} · 校验和 {{ selectedDocumentState.checksumPrefix }}…
                </p>
              </div>
              <div class="documents-content-actions">
                <div class="documents-view-switch" aria-label="文档查看模式">
                  <button
                    type="button"
                    :class="{ active: documentViewMode === 'rendered' }"
                    :aria-pressed="documentViewMode === 'rendered'"
                    @click="documentViewMode = 'rendered'"
                  >
                    预览
                  </button>
                  <button
                    type="button"
                    :class="{ active: documentViewMode === 'source' }"
                    :aria-pressed="documentViewMode === 'source'"
                    @click="documentViewMode = 'source'"
                  >
                    原文
                  </button>
                </div>
                <button
                  v-if="selectedDocumentState.syncStatus === 'FORMAT_ERROR' || !selectedDocumentState.fileExists"
                  class="primary-button compact"
                  type="button"
                  :disabled="isWorking"
                  @click="repairDocument"
                >
                  自动修复
                </button>
              </div>
            </header>
            <p v-if="!selectedDocumentState.fileExists" class="documents-warning">
              本地文件缺失，可从数据库镜像恢复。
            </p>
            <p v-else-if="selectedDocumentState.syncStatus === 'FORMAT_ERROR'" class="documents-warning">
              文件格式错误：修复将保留正文并恢复 YAML 头；正文无法提取时从镜像恢复整份文件。
            </p>
            <div
              v-if="documentDetail && documentViewMode === 'rendered'"
              class="documents-viewer documents-markdown markdown-content"
              v-html="renderProjectDocumentMarkdown(documentDetail.content)"
            ></div>
            <pre v-else-if="documentDetail" class="documents-viewer">{{ stripYamlHeader(documentDetail.content) }}</pre>
            <p v-else class="documents-loading">正在加载…</p>
          </template>

          <div v-else class="documents-zero-state documents-zero-state-inline">
            <span class="documents-zero-icon" aria-hidden="true">◇</span>
            <h3>{{ emptyStateTitle }}</h3>
            <p>{{ emptyStateDescription }}</p>
          </div>
        </section>
      </div>
    </template>

    <!-- Embedding 隐私提示（开启前必须确认） -->
    <div v-if="showPrivacyNotice" class="dialog-backdrop" @click.self="showPrivacyNotice = false">
      <div class="dialog-card">
        <h3>开启项目文档云端嵌入？</h3>
        <p>
          开启后，五份项目文档的内容可能发送给当前配置的 Embedding
          模型服务用于生成向量索引。关闭后仍保留本地全文检索能力。
        </p>
        <div class="dialog-actions">
          <button class="ghost-button" type="button" @click="showPrivacyNotice = false">取消</button>
          <button class="primary-button compact" type="button" @click="confirmEnableEmbedding">确认开启</button>
        </div>
      </div>
    </div>

    <p v-if="errorMessage" class="documents-message documents-error" role="alert">{{ errorMessage }}</p>
    <p v-if="successMessage" class="documents-message documents-success" role="status">{{ successMessage }}</p>
  </div>
</template>

<style scoped>
.documents-page {
  display: flex;
  flex-direction: column;
  gap: 20px;
  width: min(1440px, 100%);
  height: calc(100vh - 78px);
  min-height: 0;
  margin: 0 auto;
  padding: 32px 40px 48px;
  overflow-y: auto;
}

.documents-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-end;
  gap: 24px;
}

.documents-heading {
  min-width: 0;
}

.documents-heading small {
  color: var(--muted);
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.14em;
}

.documents-heading h2 {
  margin: 6px 0 0;
  font-size: 24px;
  line-height: 1.2;
  letter-spacing: 0;
}

.documents-subtitle {
  margin: 8px 0 0;
  color: var(--muted);
  font-size: 13px;
  line-height: 1.5;
}

.documents-actions {
  display: flex;
  gap: 10px;
  align-items: flex-end;
  flex-shrink: 0;
}

.documents-project-picker {
  display: grid;
  gap: 6px;
}

.documents-project-picker > span {
  color: var(--muted);
  font-size: 11px;
  font-weight: 600;
}

.documents-project-select {
  width: 220px;
  height: 42px;
  padding: 0 36px 0 14px;
  border: 1px solid var(--line);
  border-radius: 10px;
  outline: none;
  color: var(--ink);
  background: var(--surface);
  font-size: 13px;
  font-weight: 600;
  transition: border-color var(--transition-fast), box-shadow var(--transition-fast);
}

.documents-project-select:focus {
  border-color: var(--primary);
  box-shadow: 0 0 0 4px var(--primary-soft);
}

.documents-refresh-button {
  display: inline-flex;
  height: 42px;
  align-items: center;
  gap: 7px;
  padding: 0 15px;
  border: 1px solid var(--line);
  border-radius: 10px;
  color: var(--ink);
  background: var(--surface);
  font-size: 12px;
  font-weight: 700;
  transition: color var(--transition-fast), border-color var(--transition-fast), background var(--transition-fast);
}

.documents-refresh-button:hover:not(:disabled) {
  border-color: var(--primary);
  color: var(--primary);
  background: var(--primary-soft);
}

.documents-refresh-button > span {
  font-size: 17px;
  line-height: 1;
}

.documents-refresh-button > span.spinning {
  animation: documents-spin 0.7s linear infinite;
}

@keyframes documents-spin {
  to { transform: rotate(360deg); }
}

.documents-meta {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 20px;
  min-height: 62px;
  padding: 12px 16px;
  border: 1px solid var(--line);
  border-radius: 12px;
  color: var(--muted);
  background: color-mix(in srgb, var(--surface) 86%, transparent);
  box-shadow: var(--shadow-sm);
  font-size: 12px;
}

.documents-meta-summary {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.documents-status {
  display: inline-flex;
  min-height: 26px;
  align-items: center;
  padding: 0 10px;
  border-radius: var(--radius-pill);
  color: var(--primary);
  background: var(--primary-soft);
  font-size: 11px;
  font-weight: 700;
}

.documents-status[data-status='ACTIVE'] {
  color: var(--success);
  background: var(--success-soft);
}

.documents-status[data-status='PROMOTING'] {
  color: var(--warning);
  background: var(--warning-soft);
}

.documents-workspace {
  display: inline-flex;
  max-width: min(440px, 42vw);
  align-items: center;
  gap: 7px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: var(--font-mono);
  font-size: 11px;
}

.documents-workspace b {
  color: var(--ink-secondary);
  font-family: var(--font-sans);
  font-size: 11px;
  font-weight: 600;
}

.documents-workspace span {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
}

.documents-embedding {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  flex-shrink: 0;
  cursor: pointer;
}

.documents-embedding-copy {
  display: grid;
  gap: 2px;
  text-align: right;
}

.documents-embedding-copy strong {
  color: var(--ink);
  font-size: 12px;
}

.documents-embedding-copy small {
  color: var(--muted);
  font-size: 10px;
}

.documents-embedding-input {
  position: absolute;
  width: 1px;
  height: 1px;
  overflow: hidden;
  opacity: 0;
}

.documents-embedding-track {
  position: relative;
  width: 38px;
  height: 22px;
  border: 1px solid var(--line);
  border-radius: var(--radius-pill);
  background: var(--gray-200);
  transition: background var(--transition-fast), border-color var(--transition-fast);
}

.documents-embedding-track i {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: #ffffff;
  box-shadow: 0 1px 4px rgba(0, 0, 0, 0.22);
  transition: transform var(--transition-fast);
}

.documents-embedding-input:checked + .documents-embedding-track {
  border-color: var(--success);
  background: var(--success);
}

.documents-embedding-input:checked + .documents-embedding-track i {
  transform: translateX(16px);
}

.documents-embedding-input:focus-visible + .documents-embedding-track {
  outline: 2px solid var(--primary);
  outline-offset: 2px;
}

.documents-embedding-input:disabled + .documents-embedding-track {
  cursor: not-allowed;
  opacity: 0.45;
}

.documents-body {
  display: grid;
  flex: 1;
  min-height: 360px;
  grid-template-columns: 250px minmax(0, 1fr);
  overflow: hidden;
  border: 1px solid var(--line);
  border-radius: 12px;
  background: var(--surface);
  box-shadow: var(--shadow-sm);
}

.documents-body.is-empty {
  grid-template-columns: minmax(0, 1fr);
}

.documents-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-height: 0;
  padding: 12px;
  overflow-y: auto;
  border-right: 1px solid var(--line);
  background: var(--surface-soft);
}

.documents-item {
  display: grid;
  gap: 4px;
  min-height: 76px;
  padding: 12px;
  border: 1px solid transparent;
  border-radius: 8px;
  color: var(--ink);
  background: transparent;
  text-align: left;
  transition: border-color var(--transition-fast), background var(--transition-fast);
}

.documents-item:hover {
  background: color-mix(in srgb, var(--surface) 72%, transparent);
}

.documents-item.active {
  border-color: color-mix(in srgb, var(--primary) 32%, var(--line));
  background: var(--surface);
  box-shadow: 0 4px 14px rgba(0, 122, 255, 0.08);
}

.documents-item-name {
  color: var(--ink);
  font-size: 13px;
  font-weight: 600;
}

.documents-item-file {
  overflow: hidden;
  color: var(--muted);
  font-size: 12px;
  font-family: var(--font-mono);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.documents-item-status {
  color: var(--muted);
  font-size: 12px;
}

.documents-item-status.approved {
  color: var(--success);
}

.documents-item-status.error {
  color: var(--danger);
}

.documents-item-status.missing {
  color: var(--warning);
}

.documents-list-footer {
  display: flex;
  flex-direction: column;
  gap: 10px;
  margin-top: auto;
  padding: 12px 4px 4px;
}

.documents-hint {
  margin: 0;
  color: var(--muted);
  font-size: 12px;
  line-height: 1.5;
}

.documents-content {
  display: flex;
  flex-direction: column;
  gap: 16px;
  min-width: 0;
  min-height: 0;
  padding: 20px;
  overflow: hidden;
}

.documents-content-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
  flex-shrink: 0;
}

.documents-content-header h3 {
  margin: 0;
  font-size: 16px;
  letter-spacing: 0;
}

.documents-content-header h3 small {
  display: inline-block;
  margin-left: 8px;
  color: var(--muted);
  font-weight: 400;
  font-family: var(--font-mono);
  font-size: 12px;
}

.documents-reason {
  margin: 4px 0 0;
  color: var(--muted);
  font-size: 12px;
}

.documents-content-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}

.documents-view-switch {
  display: inline-grid;
  grid-template-columns: repeat(2, 1fr);
  padding: 3px;
  border: 1px solid var(--line);
  border-radius: 9px;
  background: var(--surface-soft);
}

.documents-view-switch button {
  min-width: 48px;
  height: 28px;
  padding: 0 10px;
  border: 0;
  border-radius: 6px;
  color: var(--muted);
  background: transparent;
  font-size: 11px;
  font-weight: 600;
  transition: color var(--transition-fast), background var(--transition-fast), box-shadow var(--transition-fast);
}

.documents-view-switch button.active {
  color: var(--ink);
  background: var(--surface);
  box-shadow: 0 1px 4px rgba(0, 0, 0, 0.08);
}

.documents-viewer,
.documents-editor {
  flex: 1;
  width: 100%;
  min-height: 260px;
  margin: 0;
  padding: 18px;
  overflow: auto;
  border: 1px solid var(--line);
  border-radius: 8px;
  color: var(--ink-secondary);
  background: var(--surface-soft);
  font-family: var(--font-mono);
  font-size: 13px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
}

.documents-editor {
  outline: none;
  resize: vertical;
}

.documents-markdown {
  color: var(--ink-secondary);
  font-family: var(--font-sans);
  line-height: 1.7;
  white-space: normal;
  word-break: normal;
}

.documents-markdown :deep(h1),
.documents-markdown :deep(h2),
.documents-markdown :deep(h3),
.documents-markdown :deep(h4) {
  letter-spacing: 0;
}

.documents-markdown :deep(h1) {
  font-size: 22px;
}

.documents-markdown :deep(h2) {
  font-size: 18px;
}

.documents-markdown :deep(h3) {
  font-size: 15px;
}

.documents-loading {
  color: var(--muted);
  font-size: 13px;
  padding: 24px;
  text-align: center;
}

.documents-zero-state {
  display: grid;
  min-height: 360px;
  place-content: center;
  justify-items: center;
  padding: 40px 24px;
  border: 1px solid var(--line);
  border-radius: 12px;
  color: var(--muted);
  background: var(--surface);
  box-shadow: var(--shadow-sm);
  text-align: center;
}

.documents-zero-state-inline {
  flex: 1;
  min-height: 280px;
  border: 0;
  border-radius: 0;
  box-shadow: none;
}

.documents-zero-icon {
  display: grid;
  width: 48px;
  height: 48px;
  place-items: center;
  margin-bottom: 16px;
  border: 1px solid color-mix(in srgb, var(--primary) 24%, var(--line));
  border-radius: 14px;
  color: var(--primary);
  background: var(--primary-soft);
  font-size: 22px;
}

.documents-zero-state h3 {
  margin: 0;
  color: var(--ink);
  font-size: 17px;
  letter-spacing: 0;
}

.documents-zero-state p {
  max-width: 460px;
  margin: 8px 0 0;
  font-size: 13px;
  line-height: 1.6;
}

.documents-warning {
  margin: 0;
  padding: 10px 12px;
  border-radius: 8px;
  color: var(--warning);
  background: var(--warning-soft);
  font-size: 13px;
}

.documents-message {
  position: fixed;
  right: 24px;
  bottom: 24px;
  z-index: 70;
  max-width: 420px;
  margin: 0;
  padding: 12px 16px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--surface-elevated);
  box-shadow: var(--shadow);
  font-size: 13px;
}

.documents-error {
  border-color: color-mix(in srgb, var(--danger) 30%, var(--line));
  color: var(--danger);
}

.documents-success {
  border-color: color-mix(in srgb, var(--success) 30%, var(--line));
  color: var(--success);
}

.documents-page :deep(.ghost-button) {
  height: 36px;
  padding: 0 14px;
  border: 1px solid var(--line);
  border-radius: 8px;
  color: var(--ink-secondary);
  background: var(--surface);
  font-size: 12px;
  font-weight: 600;
  transition: color var(--transition-fast), border-color var(--transition-fast), background var(--transition-fast);
}

.documents-page :deep(.ghost-button:hover:not(:disabled)) {
  border-color: var(--primary);
  color: var(--primary);
  background: var(--primary-soft);
}

.dialog-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.45);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 60;
}

.dialog-card {
  width: min(440px, calc(100vw - 40px));
  max-width: 440px;
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 24px;
  border: 1px solid var(--line);
  border-radius: 12px;
  background: var(--surface-elevated);
  box-shadow: var(--shadow-lg);
}

.dialog-card h3 {
  margin: 0;
  font-size: 18px;
  letter-spacing: 0;
}

.dialog-card p {
  margin: 0;
  color: var(--muted);
  font-size: 13px;
  line-height: 1.6;
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

@media (max-width: 980px) {
  .documents-page {
    padding: 28px;
  }

  .documents-body {
    grid-template-columns: 220px minmax(0, 1fr);
  }

  .documents-workspace {
    max-width: 280px;
  }
}

@media (max-width: 760px) {
  .documents-page {
    height: auto;
    min-height: calc(100vh - 78px);
    padding: 24px 20px 40px;
    overflow: visible;
  }

  .documents-header {
    align-items: stretch;
    flex-direction: column;
    gap: 18px;
  }

  .documents-actions {
    align-items: flex-end;
  }

  .documents-project-picker {
    flex: 1;
  }

  .documents-project-select {
    width: 100%;
  }

  .documents-meta {
    align-items: stretch;
    flex-direction: column;
    gap: 14px;
  }

  .documents-meta-summary {
    align-items: flex-start;
    flex-direction: column;
    gap: 8px;
  }

  .documents-workspace {
    max-width: 100%;
  }

  .documents-embedding {
    justify-content: space-between;
    padding-top: 12px;
    border-top: 1px solid var(--line);
  }

  .documents-embedding-copy {
    text-align: left;
  }

  .documents-body {
    display: block;
    min-height: 0;
    overflow: visible;
  }

  .documents-list {
    flex-direction: row;
    padding: 10px;
    overflow-x: auto;
    border-right: 0;
    border-bottom: 1px solid var(--line);
  }

  .documents-item {
    min-width: 180px;
  }

  .documents-list-footer {
    min-width: 200px;
    margin-top: 0;
    padding: 8px;
  }

  .documents-content {
    min-height: 340px;
    padding: 16px;
    overflow: visible;
  }
}

@media (max-width: 520px) {
  .documents-heading h2 {
    font-size: 21px;
  }

  .documents-refresh-button {
    width: 42px;
    justify-content: center;
    padding: 0;
    font-size: 0;
  }

  .documents-refresh-button > span {
    font-size: 18px;
  }

  .documents-content-header {
    flex-direction: column;
  }

  .documents-content-actions {
    width: 100%;
    flex-wrap: wrap;
    justify-content: flex-end;
  }

  .documents-view-switch {
    margin-right: auto;
  }

  .documents-content-header h3 small {
    display: block;
    margin: 5px 0 0;
  }
}
</style>
