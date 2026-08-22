<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'

import { apiFetch, ApiRequestError } from '../api'
import type { PromptReport } from '../api'
import CenteredConfirmDialog from '../components/CenteredConfirmDialog.vue'

/**
 * AI 全局提示词查看弹窗（执行计划 §21.5）。
 *
 * - 展示客户端适配后的完整只读提示词（复制与安装使用同一生成结果）。
 * - 独立操作：复制提示词（带成功/失败反馈）、重新生成、安装（用户确认后写入）。
 * - 不支持自动安装的客户端隐藏安装操作，但保留查看与复制。
 */

const props = defineProps<{
  visible: boolean
  clientType: string
  clientDisplayName: string
}>()

const emit = defineEmits<{
  (event: 'close'): void
}>()

const report = ref<PromptReport | null>(null)
const isLoading = ref(false)
const isInstalling = ref(false)
const copyFeedback = ref('')
const errorMessage = ref('')
/** 安装提示词前的应用内居中确认框可见状态。 */
const showInstallConfirmation = ref(false)

const toolNameHint = computed(() => {
  switch (report.value?.clientType) {
    case 'Claude':
      return '该客户端实际工具名形如 mcp__memstack__project_handoff_get'
    case 'Cursor':
      return '该客户端实际工具名形如 memstack_project_handoff_get'
    case 'Codex':
      return '该客户端实际工具名形如 mcp__memstack__project_handoff_get'
    default:
      return '通用提示词使用 memstack.工具名 表达逻辑归属'
  }
})

watch(copyFeedback, (next) => {
  if (next.length === 0) return
  window.setTimeout(() => {
    if (copyFeedback.value === next) copyFeedback.value = ''
  }, 2500)
})

watch(
  () => [props.visible, props.clientType],
  ([visible]) => {
    if (visible) {
      void load()
      return
    }
    showInstallConfirmation.value = false
  },
)

onMounted(() => {
  if (props.visible) void load()
})

async function load(): Promise<void> {
  if (!props.clientType) return
  isLoading.value = true
  errorMessage.value = ''
  try {
    report.value = await apiFetch<PromptReport>(
      `/api/ai-prompt/preview?clientType=${encodeURIComponent(props.clientType)}`,
      { method: 'GET' },
      null,
    )
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isLoading.value = false
  }
}

async function copyPrompt(): Promise<void> {
  const text = report.value?.promptText
  if (!text) return
  try {
    await navigator.clipboard.writeText(text)
    copyFeedback.value = '已复制完整提示词'
  } catch {
    copyFeedback.value = '复制失败：请手动选中文本复制'
  }
}

/** 关闭提示词弹窗并清理尚未确认的安装操作。 */
function closeDialog(): void {
  showInstallConfirmation.value = false
  emit('close')
}

/** 打开应用内安装确认框。 */
function openInstallConfirmation(): void {
  if (!report.value || !props.clientType) return
  showInstallConfirmation.value = true
}

/** 取消安装并关闭居中确认框。 */
function closeInstallConfirmation(): void {
  if (isInstalling.value) return
  showInstallConfirmation.value = false
}

/** 用户确认后安装提示词。 */
async function confirmInstall(): Promise<void> {
  if (!report.value || !props.clientType || isInstalling.value) return
  isInstalling.value = true
  errorMessage.value = ''
  try {
    report.value = await apiFetch<PromptReport>(
      '/api/ai-prompt/install',
      { method: 'POST', body: JSON.stringify({ clientType: props.clientType }) },
      null,
    )
    showInstallConfirmation.value = false
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isInstalling.value = false
  }
}

function readError(error: unknown): string {
  if (error instanceof ApiRequestError) return error.message
  return '请求失败，请稍后重试'
}
</script>

<template>
  <div v-if="visible" class="prompt-backdrop" @click.self="closeDialog">
    <div class="prompt-dialog">
      <header class="prompt-header">
        <div>
          <h3>MemStack 全局提示词 · {{ clientDisplayName }}</h3>
          <p class="prompt-meta">
            MCP 服务标识：<code>{{ report?.serviceId ?? 'memstack' }}</code>
            · 模板版本 v{{ report?.templateVersion ?? '-' }}
          </p>
          <p class="prompt-meta">{{ toolNameHint }}</p>
        </div>
        <button class="prompt-close" type="button" title="关闭" @click="closeDialog">✕</button>
      </header>

      <p v-if="isLoading" class="prompt-loading">正在生成当前客户端适配的提示词…</p>
      <template v-else-if="report">
        <p class="prompt-diff" :class="{ installed: report.installed }">{{ report.diffText }}</p>
        <p v-if="report.configPath" class="prompt-path">配置目标：{{ report.configPath }}</p>
        <pre class="prompt-body">{{ report.promptText }}</pre>
        <footer class="prompt-footer">
          <span v-if="copyFeedback" class="prompt-feedback" :class="{ failed: copyFeedback.includes('失败') }">
            {{ copyFeedback }}
          </span>
          <span v-if="errorMessage" class="prompt-error">{{ errorMessage }}</span>
          <div class="prompt-actions">
            <button class="prompt-secondary-button" type="button" :disabled="isLoading" @click="load">重新生成</button>
            <button class="prompt-secondary-button" type="button" @click="copyPrompt">复制提示词</button>
            <button
              v-if="report.canInstall"
              class="prompt-primary-button"
              type="button"
              :disabled="isInstalling"
              @click="openInstallConfirmation"
            >
              {{ isInstalling ? '安装中…' : report.installed ? '重新安装（幂等）' : '安装' }}
            </button>
          </div>
        </footer>
      </template>
      <p v-else class="prompt-loading">{{ errorMessage || '暂无提示词' }}</p>
    </div>

    <CenteredConfirmDialog
      v-if="report"
      :visible="showInstallConfirmation"
      title="安装 MemStack 提示词？"
      description="提示词将写入以下配置文件："
      :detail="report.configPath ?? '目标配置'"
      note="安装前会自动备份现有文件，与 MemStack 无关的用户配置将完整保留。"
      confirm-label="确认安装"
      pending-label="安装中…"
      :is-pending="isInstalling"
      @cancel="closeInstallConfirmation"
      @confirm="confirmInstall"
    />
  </div>
</template>

<style scoped>
.prompt-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 70;
}

.prompt-dialog {
  width: min(720px, 92vw);
  max-height: 84vh;
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 20px;
  border: 1px solid var(--line);
  border-radius: 14px;
  color: var(--ink);
  background: var(--surface-elevated);
  box-shadow: var(--shadow-lg);
}

:global(.theme-dark) .prompt-dialog {
  background: #1d2330;
  box-shadow: 0 18px 48px rgba(0, 0, 0, 0.4);
}

.prompt-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
}

.prompt-header h3 {
  margin: 0;
  font-size: 17px;
}

.prompt-meta {
  margin: 6px 0 0;
  font-size: 12px;
  color: var(--muted);
}

.prompt-meta code {
  font-family: var(--mono, monospace);
  color: var(--accent, #2f7ec9);
}

.prompt-close {
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 16px;
  cursor: pointer;
  padding: 4px;
}

.prompt-diff {
  margin: 0;
  padding: 8px 12px;
  border-radius: 8px;
  background: rgba(64, 158, 255, 0.1);
  color: var(--accent, #2f7ec9);
  font-size: 13px;
}

.prompt-diff.installed {
  background: rgba(46, 164, 108, 0.12);
  color: #2ea46c;
}

.prompt-path {
  margin: 0;
  font-size: 12px;
  color: var(--muted);
  font-family: var(--mono, monospace);
  word-break: break-all;
}

.prompt-body {
  flex: 1;
  min-height: 220px;
  margin: 0;
  padding: 14px;
  border-radius: 10px;
  border: 1px solid var(--line);
  color: var(--ink-secondary);
  background: var(--surface-soft);
  font-family: var(--mono, monospace);
  font-size: 12.5px;
  line-height: 1.7;
  white-space: pre-wrap;
  overflow: auto;
  user-select: text;
}

.prompt-loading {
  color: var(--muted);
  font-size: 13px;
  text-align: center;
  padding: 24px 0;
}

.prompt-footer {
  display: flex;
  align-items: center;
  gap: 12px;
}

.prompt-feedback {
  font-size: 12px;
  color: #2ea46c;
  margin-right: auto;
}

.prompt-feedback.failed {
  color: #e06c60;
}

.prompt-error {
  font-size: 12px;
  color: #e06c60;
  margin-right: auto;
}

.prompt-actions {
  display: flex;
  gap: 8px;
  margin-left: auto;
}

.prompt-secondary-button {
  padding: 7px 14px;
  border-radius: 8px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--ink);
  cursor: pointer;
  font-size: 13px;
}

.prompt-secondary-button:disabled {
  opacity: 0.5;
  cursor: default;
}

.prompt-primary-button {
  padding: 7px 16px;
  border-radius: 8px;
  border: none;
  background: var(--accent, #2f7ec9);
  color: #fff;
  cursor: pointer;
  font-size: 13px;
  font-weight: 600;
}

.prompt-primary-button:disabled {
  opacity: 0.6;
  cursor: default;
}

</style>
