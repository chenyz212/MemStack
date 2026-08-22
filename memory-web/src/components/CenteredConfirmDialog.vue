<script setup lang="ts">
/**
 * 应用内居中确认框。
 *
 * 只负责展示确认信息并发出取消、确认事件，具体业务操作由调用方执行。
 */

defineProps<{
  visible: boolean
  title: string
  description: string
  detail: string
  note: string
  confirmLabel: string
  pendingLabel: string
  isPending: boolean
}>()

const emit = defineEmits<{
  (event: 'cancel'): void
  (event: 'confirm'): void
}>()
</script>

<template>
  <div
    v-if="visible"
    class="modal-backdrop confirmation-backdrop"
    @click.self="emit('cancel')"
  >
    <section
      class="confirmation-card centered-confirm-card"
      role="dialog"
      aria-modal="true"
      aria-labelledby="centered-confirm-title"
    >
      <button
        class="confirmation-close"
        type="button"
        aria-label="关闭确认框"
        :disabled="isPending"
        @click="emit('cancel')"
      >
        ×
      </button>
      <span class="confirmation-icon centered-confirm-icon" aria-hidden="true">↓</span>
      <small>CONFIRM ACTION</small>
      <h2 id="centered-confirm-title">{{ title }}</h2>
      <p>{{ description }}</p>
      <p class="centered-confirm-detail">{{ detail }}</p>
      <p class="centered-confirm-note">{{ note }}</p>
      <div class="confirmation-actions">
        <button
          class="centered-confirm-secondary"
          type="button"
          :disabled="isPending"
          @click="emit('cancel')"
        >
          取消
        </button>
        <button
          class="centered-confirm-primary"
          type="button"
          :disabled="isPending"
          @click="emit('confirm')"
        >
          {{ isPending ? pendingLabel : confirmLabel }}
        </button>
      </div>
    </section>
  </div>
</template>

<style scoped>
.centered-confirm-card {
  width: min(460px, 92vw);
}

.centered-confirm-icon {
  color: var(--primary);
  background: var(--primary-soft);
  box-shadow: var(--shadow-glow);
}

.centered-confirm-detail {
  margin-top: 10px;
  padding: 10px 12px;
  border: 1px solid var(--line);
  border-radius: 8px;
  color: var(--ink-secondary);
  background: var(--surface-soft);
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.6;
  overflow-wrap: anywhere;
}

.centered-confirm-note {
  margin-top: 12px;
}

.centered-confirm-secondary,
.centered-confirm-primary {
  min-height: 38px;
  padding: 0 14px;
  border-radius: 8px;
  font-size: 13px;
  font-weight: 600;
}

.centered-confirm-secondary {
  border: 1px solid var(--line);
  color: var(--ink);
  background: var(--surface);
}

.centered-confirm-primary {
  border: 0;
  color: var(--primary-foreground);
  background: var(--primary);
}
</style>
