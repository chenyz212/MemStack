<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'

import { apiFetch, ApiRequestError } from '../api'
import type { ConclusionCardPayload, MemoryCandidateItem } from '../api'

/**
 * 结论卡片候选的结构化字段编辑器（执行计划 §21.4）。
 *
 * 挂载在候选编辑流程中：必填校验、数组项增删、重要度评估及理由、
 * Embedding 选择、问题编号关联；保存走候选乐观锁并重渲染 Markdown 正文。
 */

const props = defineProps<{
  candidateId: string
  expectedVersion: number
}>()

const emit = defineEmits<{
  (event: 'updated', candidate: MemoryCandidateItem): void
}>()

/** 单行文本数组字段定义（标签 + 字段名 + 是否必填提示）。 */
const LIST_FIELDS: { key: keyof ConclusionCardPayload; label: string; placeholder: string; required?: boolean }[] = [
  { key: 'evidence', label: '证据', placeholder: '例如：崩溃堆栈指向 ConfigParser', required: true },
  { key: 'verifiedResults', label: '已验证结果', placeholder: '例如：修复后连续启动 10 次成功', required: true },
  { key: 'applicableConditions', label: '适用条件', placeholder: '例如：同类启动流程' },
  { key: 'notApplicableConditions', label: '不适用条件', placeholder: '例如：纯前端项目' },
  { key: 'failedAttempts', label: '失败方案（说明方案与失败原因）', placeholder: '例如：延迟整体启动——未解决，仅推迟崩溃' },
  { key: 'doNotRepeat', label: '不要重复', placeholder: '例如：不要在日志初始化前解析配置' },
  { key: 'retryConditions', label: '重新尝试条件', placeholder: '例如：若重构启动顺序可重新评估' },
  { key: 'nextSteps', label: '下一步', placeholder: '例如：补充启动顺序回归测试' },
]

const TEXT_FIELDS: { key: keyof ConclusionCardPayload; label: string; placeholder: string; required?: boolean; multiline?: boolean }[] = [
  { key: 'problemDescription', label: '问题描述', placeholder: '问题现象与背景', required: true, multiline: true },
  { key: 'finalConclusion', label: '最终结论', placeholder: '一句话结论', required: true, multiline: true },
  { key: 'rootCause', label: '根本原因', placeholder: '被证据支持的根因', required: true, multiline: true },
  { key: 'importanceReason', label: '重要度评估理由', placeholder: '为什么是这个重要度', required: true },
]

const payload = ref<ConclusionCardPayload | null>(null)
const isLoading = ref(false)
const isSaving = ref(false)
const errorMessage = ref('')
const saveSuccess = ref(false)

const missingRequired = computed(() => {
  const value = payload.value
  if (!value) return true
  if (!value.title.trim()) return true
  if (!value.problemDescription.trim() || !value.finalConclusion.trim() || !value.rootCause.trim()) return true
  if (value.evidence.every((item) => !item.trim())) return true
  if (value.verifiedResults.every((item) => !item.trim())) return true
  if (!value.importanceReason.trim()) return true
  if (!value.resolvedAt.trim()) return true
  return false
})

watch(saveSuccess, (next) => {
  if (!next) return
  window.setTimeout(() => {
    saveSuccess.value = false
  }, 2500)
})

async function load(): Promise<void> {
  isLoading.value = true
  try {
    payload.value = await apiFetch<ConclusionCardPayload | null>(
      `/api/memory-candidates/${props.candidateId}/conclusion-payload`,
      { method: 'GET' },
      null,
    )
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isLoading.value = false
  }
}

onMounted(load)
watch(() => props.candidateId, load)

function addItem(field: keyof ConclusionCardPayload): void {
  const value = payload.value
  if (!value) return
  ;(value[field] as string[]).push('')
}

/** 重要度 1-5 的可读标签（与后端结论卡片规范一致）。 */
function levelLabel(level: number): string {
  const labels: Record<number, string> = {
    1: '随手记录',
    2: '低',
    3: '中',
    4: '高',
    5: '关键教训',
  }
  return labels[level] ?? ''
}

/** 按字段名写入单行/多行文本字段（绕开联合键索引的 never 推断）。 */
function setTextField(key: keyof ConclusionCardPayload, value: string): void {
  const current = payload.value
  if (!current) return
  ;(current as unknown as Record<string, unknown>)[key] = value
}

function removeItem(field: keyof ConclusionCardPayload, index: number): void {
  const value = payload.value
  if (!value) return
  ;(value[field] as string[]).splice(index, 1)
}

function moveItem(field: keyof ConclusionCardPayload, index: number, offset: -1 | 1): void {
  const value = payload.value
  if (!value) return
  const list = value[field] as string[]
  const target = index + offset
  if (target < 0 || target >= list.length) return
  const [entry] = list.splice(index, 1)
  list.splice(target, 0, entry)
}

async function save(): Promise<void> {
  const value = payload.value
  if (!value || missingRequired.value) return
  isSaving.value = true
  try {
    // 数组规范化：去首尾空白、剔除空项（后端按显式数组保存）。
    const normalized: ConclusionCardPayload = {
      ...value,
      applicableConditions: clean(value.applicableConditions),
      notApplicableConditions: clean(value.notApplicableConditions),
      evidence: clean(value.evidence),
      verifiedResults: clean(value.verifiedResults),
      failedAttempts: clean(value.failedAttempts),
      doNotRepeat: clean(value.doNotRepeat),
      retryConditions: clean(value.retryConditions),
      nextSteps: clean(value.nextSteps),
      keywords: clean(value.keywords),
      tags: clean(value.tags),
    }
    const updated = await apiFetch<MemoryCandidateItem>(
      `/api/memory-candidates/${props.candidateId}/conclusion-payload`,
      {
        method: 'PUT',
        body: JSON.stringify({ payload: normalized, expectedVersion: props.expectedVersion }),
      },
      null,
    )
    payload.value = normalized
    saveSuccess.value = true
    emit('updated', updated)
  } catch (error) {
    errorMessage.value = readError(error)
  } finally {
    isSaving.value = false
  }
}

function clean(list: string[]): string[] {
  return list.map((item) => item.trim()).filter((item) => item.length > 0)
}

function readError(error: unknown): string {
  if (error instanceof ApiRequestError) {
    if (error.code === 'MEMORY_CANDIDATE_VERSION_CONFLICT') return '候选已被其他操作修改，请刷新后重试'
    if (error.code === 'CONCLUSION_CARD_FIELD_REQUIRED') return error.message
    return error.message
  }
  return '请求失败，请稍后重试'
}
</script>

<template>
  <section v-if="isLoading" class="card-loading">正在加载结构化字段…</section>
  <section v-else-if="payload" class="card-editor">
    <header class="card-editor-header">
      <h4>结论卡片结构化字段</h4>
      <p class="card-editor-hint">
        必填：问题描述 / 最终结论 / 根本原因 / 证据 / 已验证结果 / 重要度理由。保存后正文将按字段重新渲染。
      </p>
    </header>

    <div class="card-field">
      <label>标题</label>
      <input v-model="payload.title" type="text" placeholder="建议「问题 → 结论」式标题" />
    </div>

    <div class="card-field-row">
      <div class="card-field">
        <label>关联问题编号（可留空）</label>
        <input v-model.trim="payload.problemId" type="text" placeholder="PROB-20260821-001" />
      </div>
      <div class="card-field">
        <label>解决时间</label>
        <input v-model.trim="payload.resolvedAt" type="text" placeholder="2026-08-21T08:00:00Z" />
      </div>
    </div>

    <div v-for="field in TEXT_FIELDS" :key="field.key" class="card-field">
      <label>{{ field.label }}<em v-if="field.required" class="card-required">*</em></label>
      <textarea
        v-if="field.multiline"
        :value="(payload[field.key] as string)"
        rows="2"
        :placeholder="field.placeholder"
        @input="setTextField(field.key, ($event.target as HTMLTextAreaElement).value)"
      ></textarea>
      <input
        v-else
        :value="(payload[field.key] as string)"
        type="text"
        :placeholder="field.placeholder"
        @input="setTextField(field.key, ($event.target as HTMLInputElement).value)"
      />
    </div>

    <div class="card-field-row">
      <div class="card-field">
        <label>重要度（1-5）</label>
        <select
          :value="payload.importance"
          @change="payload.importance = Number(($event.target as HTMLSelectElement).value)"
        >
          <option v-for="level in 5" :key="level" :value="level">{{ level }}（{{ levelLabel(level) }}）</option>
        </select>
      </div>
      <div class="card-field card-field-checkbox">
        <label>允许云端嵌入（卡片自身开关，不继承项目设置）</label>
        <input v-model="payload.cloudEmbeddingAllowed" type="checkbox" />
      </div>
    </div>

    <div v-for="field in LIST_FIELDS" :key="field.key" class="card-field">
      <label>{{ field.label }}<em v-if="field.required" class="card-required">*</em></label>
      <div v-for="(_, index) in (payload[field.key] as string[])" :key="index" class="card-list-row">
        <input
          v-model="(payload[field.key] as string[])[index]"
          type="text"
          :placeholder="field.placeholder"
        />
        <button type="button" class="card-mini-button" title="上移" :disabled="index === 0" @click="moveItem(field.key, index, -1)">↑</button>
        <button
          type="button"
          class="card-mini-button"
          title="下移"
          :disabled="index === (payload[field.key] as string[]).length - 1"
          @click="moveItem(field.key, index, 1)"
        >↓</button>
        <button type="button" class="card-mini-button danger" title="删除" @click="removeItem(field.key, index)">✕</button>
      </div>
      <button type="button" class="card-add-button" @click="addItem(field.key)">+ 添加一条</button>
    </div>

    <div class="card-field-row">
      <div class="card-field">
        <label>关键词（逗号分隔）</label>
        <input
          :value="payload.keywords.join(', ')"
          type="text"
          placeholder="启动, 配置"
          @input="payload.keywords = ($event.target as HTMLInputElement).value.split(/[,，]/).map((item) => item.trim()).filter(Boolean)"
        />
      </div>
      <div class="card-field">
        <label>标签（逗号分隔）</label>
        <input
          :value="payload.tags.join(', ')"
          type="text"
          placeholder="后端, 稳定性"
          @input="payload.tags = ($event.target as HTMLInputElement).value.split(/[,，]/).map((item) => item.trim()).filter(Boolean)"
        />
      </div>
    </div>

    <footer class="card-editor-footer">
      <p v-if="errorMessage" class="card-error">{{ errorMessage }}</p>
      <p v-else-if="saveSuccess" class="card-success">结构化字段已保存（版本 +1）</p>
      <button class="primary-button compact" type="button" :disabled="isSaving || missingRequired" @click="save">
        {{ isSaving ? '保存中…' : '保存结构化字段' }}
      </button>
    </footer>
  </section>
</template>

<style scoped>
.card-loading {
  color: var(--text-secondary, #8a94a6);
  font-size: 13px;
  padding: 12px;
}

.card-editor {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 14px;
  border-radius: 10px;
  border: 1px solid var(--border, rgba(255, 255, 255, 0.08));
  background: var(--surface, rgba(255, 255, 255, 0.02));
}

.card-editor-header h4 {
  margin: 0;
  font-size: 14px;
}

.card-editor-hint {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--text-secondary, #8a94a6);
}

.card-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  flex: 1;
}

.card-field > label {
  font-size: 12px;
  color: var(--text-secondary, #8a94a6);
}

.card-required {
  color: #e06c60;
  font-style: normal;
  margin-left: 2px;
}

.card-field-row {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}

.card-field-checkbox label {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  cursor: pointer;
}

.card-list-row {
  display: grid;
  grid-template-columns: 1fr auto auto auto;
  gap: 4px;
  align-items: center;
}

.card-mini-button {
  padding: 4px 8px;
  border-radius: 6px;
  border: 1px solid var(--border, rgba(255, 255, 255, 0.1));
  background: transparent;
  color: var(--text-secondary, #8a94a6);
  cursor: pointer;
}

.card-mini-button:disabled {
  opacity: 0.4;
  cursor: default;
}

.card-mini-button.danger:hover:not(:disabled) {
  color: #e06c60;
  border-color: #e06c60;
}

.card-add-button {
  align-self: flex-start;
  padding: 4px 10px;
  border-radius: 6px;
  border: 1px dashed var(--border, rgba(255, 255, 255, 0.18));
  background: transparent;
  color: var(--text-secondary, #8a94a6);
  font-size: 12px;
  cursor: pointer;
}

.card-editor-footer {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 12px;
}

.card-error {
  margin: 0;
  color: #e06c60;
  font-size: 12px;
  margin-right: auto;
}

.card-success {
  margin: 0;
  color: #2ea46c;
  font-size: 12px;
  margin-right: auto;
}
</style>
