<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import Button from "primevue/button";
import InputNumber from "primevue/inputnumber";
import ProgressBar from "primevue/progressbar";
import { useAuthStore } from "../stores/auth";
import { getArchiveInterval } from "../utils/appSettings";
import { cancelHistoryDiagnostics, getHistoryDiagnostics, startHistoryDiagnostics, type HistoryDiagnosticsReport } from "../utils/historyDiagnostics";

const props = defineProps<{ archiveBusy: boolean }>();
const emit = defineEmits<{ busy: [value: boolean] }>();
const auth = useAuthStore();
const boundary = ref<number | null>(10000);
const endDate = ref("");
const notice = ref("");
const cancelling = ref(false);
const pending = ref(false);
const report = ref<HistoryDiagnosticsReport>({ version: 1, status: "idle", message: "尚未运行接口诊断", boundaryOffset: 10000, plannedRequests: 16, rows: [] });
const running = computed(() => pending.value || report.value.status === "running");
const validBoundary = computed(() => Number.isInteger(boundary.value) && Number(boundary.value) >= 0 && Number(boundary.value) <= 9999970);
const reportText = computed(() => JSON.stringify(report.value, null, 2));
let timer: ReturnType<typeof setInterval> | undefined;
let mounted = false;
let refreshing = false;
watch(running, (value) => emit("busy", value), { immediate: true });

function poll() {
  clearInterval(timer);
  if (mounted) timer = setInterval(() => { void refresh(); }, 1500);
}
async function refresh() {
  if (refreshing) return;
  refreshing = true;
  try {
    report.value = await getHistoryDiagnostics();
    if (!running.value) { clearInterval(timer); timer = undefined; }
  } catch { /* Keep the last safe report when the app is temporarily unavailable. */ }
  finally { refreshing = false; }
}
async function start() {
  if (running.value || props.archiveBusy || !auth.loggedIn || !validBoundary.value) return;
  notice.value = "";
  let endTime: number | undefined;
  if (endDate.value) {
    endTime = Math.floor(new Date(endDate.value).getTime() / 1000);
    if (!Number.isFinite(endTime) || endTime <= 0 || endTime > Date.now() / 1000) {
      notice.value = "请选择过去的日期时间，或留空使用自动时间上界。";
      return;
    }
  }
  report.value = { version: 1, status: "running", message: "正在启动诊断…", boundaryOffset: Number(boundary.value), plannedRequests: 16, rows: [] };
  cancelling.value = false;
  pending.value = true;
  // The backend also enforces mutual exclusion with archive jobs.
  try {
    const pending = startHistoryDiagnostics(Number(boundary.value), endTime, getArchiveInterval());
    poll();
    report.value = await pending;
  } catch (error) {
    notice.value = String(error);
    report.value = { ...report.value, status: "cancelled", message: "诊断未完成，请查看提示并重试。" };
  } finally {
    pending.value = false;
    clearInterval(timer);
    timer = undefined;
    cancelling.value = false;
    await refresh();
  }
}
async function stop() {
  cancelling.value = true;
  notice.value = "已请求停止，当前请求结束后停止；单次请求最长等待约 20 秒。";
  try { await cancelHistoryDiagnostics(); } catch { cancelling.value = false; notice.value = "停止请求未成功，请稍后重试。"; }
}
async function copy() {
  try { await navigator.clipboard.writeText(reportText.value); notice.value = "脱敏报告已复制，可以粘贴到聊天中。"; }
  catch { notice.value = "自动复制不可用，请展开下方报告文本，手动全选复制。"; }
}
function time(value: number | null) {
  return value ? new Date(value * 1000).toLocaleString("zh-CN", { hour12: false }) : "—";
}
onMounted(async () => { mounted = true; await refresh(); if (running.value) poll(); });
onBeforeUnmount(() => { mounted = false; clearInterval(timer); });
</script>

<template>
  <section class="surface-card diagnostics-card">
    <div class="section-heading"><div><p class="section-kicker">HISTORY API DIAGNOSTICS</p><h3>历史消息接口诊断</h3></div></div>
    <p class="diagnostic-description">在本机使用当前登录会话，检查分页边界和时间范围参数。最多 16 次请求，与归档互斥，并计入原有频率保护。</p>
    <div class="diagnostic-fields">
      <div><label for="diagnostic-boundary">待检查的边界 offset</label><InputNumber v-model="boundary" inputId="diagnostic-boundary" :min="0" :max="9999970" :maxFractionDigits="0" :useGrouping="false" :disabled="running" /></div>
      <div><label for="diagnostic-date">时间上界（可选，使用本机时区）</label><input id="diagnostic-date" v-model="endDate" type="datetime-local" :disabled="running" /></div>
    </div>
    <p class="diagnostic-description">默认对比 offset 9990、10000、10020、10030 的 1 / 10 / 30 条窗口。时间上界留空时，使用边界前参考页的最早可靠时间；无法取得时会跳过时间测试。</p>
    <div class="diagnostic-actions">
      <Button :label="running ? '诊断中…' : '开始接口诊断'" icon="pi pi-search" :disabled="running || archiveBusy || !auth.loggedIn || !validBoundary" @click="start" />
      <Button v-if="running" :label="cancelling ? '正在停止…' : '停止诊断'" severity="secondary" outlined :disabled="cancelling" @click="stop" />
      <Button label="复制脱敏报告" icon="pi pi-copy" severity="secondary" outlined :disabled="!report.rows.length" @click="copy" />
    </div>
    <p v-if="!auth.loggedIn" class="diagnostic-description">请先在程序自己的登录窗口扫码登录。</p>
    <p class="diagnostic-description">报告仅包含请求位置、状态码、数量、时间范围及对照结果，不包含 Cookie、g_tk、账号、正文、图片地址或原始响应。诊断不会写入归档内容，也不会修改续传断点。</p>
    <p role="status" aria-live="polite">{{ report.message }}<span v-if="report.rows.length">（已取得 {{ report.rows.length }} 项结果）</span></p>
    <ProgressBar v-if="running" mode="indeterminate" style="height: 6px" />
    <p v-if="notice" role="status">{{ notice }}</p>
    <div v-if="report.rows.length" class="diagnostic-table-wrap">
      <table>
        <thead><tr><th>测试</th><th>offset / 窗口</th><th>HTTP / 业务码</th><th>更多 / 总数</th><th>识别卡片</th><th>样本最早 / 最晚时间</th><th>结果</th></tr></thead>
        <tbody><tr v-for="(row, index) in report.rows" :key="index">
          <td>{{ row.label }}<small v-if="row.endTime">上界：{{ time(row.endTime) }}<br>begin_time：{{ row.beginTime }}</small></td>
          <td>{{ row.offset }} / {{ row.count }}</td>
          <td>{{ row.httpStatus ?? '—' }} / {{ row.apiCode ?? '—' }}<small>subcode：{{ row.subcode ?? '—' }}</small></td>
          <td>{{ row.hasMore === null ? '—' : row.hasMore ? '是' : '否' }} / {{ row.totalNumber ?? '—' }}</td>
          <td>{{ row.recordCount }}</td>
          <td>{{ time(row.oldestTime) }}<br>{{ time(row.newestTime) }}</td>
          <td>{{ row.note }}
            <small v-if="row.sameAsBaseline === true">与无时间条件第一页的记录相同，可能忽略了时间参数。</small>
            <small v-else-if="row.sameAsBaseline === false">与基准页记录不同；仅凭不同还不能确认突破边界。</small>
            <small v-if="row.timeUpperBoundMatches === true">全部已识别样本时间符合上界。</small>
            <small v-else-if="row.timeUpperBoundMatches === false">存在晚于查询上界的样本，可能未应用时间条件。</small>
          </td>
        </tr></tbody>
      </table>
    </div>
    <details v-if="report.rows.length"><summary>查看或手动复制脱敏报告</summary><textarea :value="reportText" readonly rows="12" aria-label="脱敏诊断报告" spellcheck="false" /></details>
  </section>
</template>

<style scoped>
.diagnostics-card { margin-top: 1.25rem; padding: 1.5rem; }
.diagnostic-description { color: var(--p-text-muted-color); font-size: .875rem; line-height: 1.7; }
.diagnostic-fields, .diagnostic-actions { display: flex; flex-wrap: wrap; gap: .75rem; margin: 1rem 0; }
.diagnostic-fields label { display: block; font-size: .875rem; font-weight: 600; margin-bottom: .4rem; }
.diagnostic-fields :deep(.p-inputnumber) { width: 13rem; }
.diagnostic-fields :deep(input), input[type="datetime-local"] { max-width: 100%; min-width: 0; }
input[type="datetime-local"] { font: inherit; padding: .55rem .75rem; border: 1px solid var(--p-content-border-color); border-radius: 6px; color: var(--p-text-color); background: var(--p-content-background); }
.diagnostic-table-wrap { overflow-x: auto; margin-top: 1rem; }
table { width: 100%; min-width: 1000px; border-collapse: collapse; font-size: .8rem; }
th, td { padding: .65rem; border-bottom: 1px solid var(--p-content-border-color); text-align: left; vertical-align: top; }
td small { display: block; margin-top: .3rem; color: var(--p-text-muted-color); }
details { margin-top: 1rem; }
summary { cursor: pointer; }
textarea { box-sizing: border-box; width: 100%; margin-top: .75rem; padding: .75rem; font-family: monospace; color: var(--p-text-color); background: var(--p-content-background); border: 1px solid var(--p-content-border-color); border-radius: 6px; }
</style>
