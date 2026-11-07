<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { invoke, listenEvent, errText } from '../api/tauri';
import { act, store } from '../store';
import { CMD, KVM_SOURCES } from '../types';
import type { KvmDevices, KvmEvent, KvmSwitchReport } from '../types';
import Card from '../components/Card.vue';
import Icon from '../components/Icon.vue';

const devices = ref<KvmDevices | null>(null);
const pairingCode = ref(''); // 本机生成的待输配对码
const joinCode = ref(''); // 输入的对端配对码
const busyPairing = ref(false);
const targetId = ref(''); // 一键切换目标（多台时）
const switching = ref(false);
const progress = ref(''); // 切换过程提示（kvm-status 事件）
const lastReport = ref<KvmSwitchReport | null>(null);
const confirmUnpair = ref(false);

async function loadDevices(): Promise<void> {
  try {
    devices.value = await invoke<KvmDevices>(CMD.kvmDevices);
    if (!targetId.value && devices.value.peers.length === 1) {
      targetId.value = devices.value.peers[0].id;
    }
  } catch (e) {
    store.status = errText(e);
    store.statusOk = false;
  }
}

// ---------- 配对 ----------

async function startPairing(): Promise<void> {
  await act(async () => {
    const code = await invoke<string>(CMD.kvmPairStart);
    pairingCode.value = code;
    return code;
  }, (code) => `配对码已生成：${code}（5 分钟内有效）`);
}

async function joinPairing(): Promise<void> {
  if (joinCode.value.length !== 6) return;
  busyPairing.value = true;
  await act(async () => {
    const peer = await invoke(CMD.kvmPairJoin, { code: joinCode.value });
    joinCode.value = '';
    pairingCode.value = '';
    await loadDevices();
    return peer;
  }, '配对成功');
  busyPairing.value = false;
}

async function removePeer(id: string): Promise<void> {
  await act(async () => {
    await invoke(CMD.kvmPeerRemove, { id });
    await loadDevices();
  }, '已解除配对');
}

async function unpairAll(): Promise<void> {
  await act(async () => {
    await invoke(CMD.kvmUnpairAll);
    confirmUnpair.value = false;
    await loadDevices();
  }, '已解除所有配对');
}

async function setSource(id: string, e: Event): Promise<void> {
  const source = (e.target as HTMLSelectElement).value;
  await act(async () => {
    await invoke(CMD.kvmPeerSetSource, { id, source });
  }, '已更新输入口');
}

// ---------- 一键切换 ----------

async function switchNow(): Promise<void> {
  if (switching.value) return;
  switching.value = true;
  lastReport.value = null;
  progress.value = '正在连接对端…';
  await act(async () => {
    const r = await invoke<KvmSwitchReport>(CMD.kvmSwitch, { peerId: targetId.value || null });
    lastReport.value = r;
    return r;
  }, (r: KvmSwitchReport) => {
    if (r.ack_ok && r.input_ok) return '切换完成';
    if (r.ack_ok) return `对方已确认，但本机显示器不支持软件切源（音频与剪贴板已完成）`;
    return '已发送切换指令，但对方未确认';
  });
  switching.value = false;
}

onMounted(async () => {
  void loadDevices();
  await listenEvent('kvm-status', (payload) => {
    const ev = payload as KvmEvent;
    progress.value = ev.message ?? '';
  });
});
</script>

<template>
  <div class="stack">
    <Card title="一键切换" icon="keyboard">
      <template #actions>
        <span class="tag" v-if="devices">本机：{{ devices.me }}</span>
      </template>
      <p v-if="devices && devices.peers.length === 0" class="muted">
        还没有已配对的设备。先在下方完成配对，然后按
        <strong>Ctrl/Cmd+Alt+K</strong> 或点下面的按钮把画面、音频、剪贴板一起切过去。
      </p>
      <div class="kvm-switch-row">
        <select v-if="devices && devices.peers.length > 1" v-model="targetId" class="input kvm-target">
          <option v-for="p in devices.peers" :key="p.id" :value="p.id">{{ p.name }}</option>
        </select>
        <button
          class="btn primary lg"
          :disabled="store.busy || switching || (devices?.peers.length ?? 0) === 0"
          @click="switchNow"
        >
          <Icon name="restore" />
          {{ switching ? '切换中…' : devices && devices.peers.length === 1 ? `切到 ${devices.peers[0].name}` : '切换到对端' }}
        </button>
      </div>
      <p class="kvm-progress" v-if="progress">{{ progress }}</p>
      <ul class="kvm-report" v-if="lastReport">
        <li :class="lastReport.input_ok ? 'ok' : 'bad'">输入源：{{ lastReport.input_msg }}</li>
        <li :class="lastReport.ack_ok ? 'ok' : 'bad'">
          对端确认：{{ lastReport.ack_ok ? '已确认' : '未确认' }}
          <span v-if="!lastReport.ack_ok">— {{ lastReport.ack_detail }}</span>
        </li>
        <li class="muted" v-if="lastReport.audio_msg">音频：{{ lastReport.audio_msg }}</li>
        <li class="muted" v-if="lastReport.clip_msg">剪贴板：{{ lastReport.clip_msg }}</li>
      </ul>
      <p class="hint">
        发起切换的一侧执行 DDC 切源；显示器不支持 DDC/CI 时会明确提示，音频与剪贴板仍会完成（部分成功优于整体失败）。
      </p>
    </Card>

    <Card title="配对" icon="link">
      <div class="kvm-pair">
        <div class="kvm-pair-col">
          <h4>这台设备发起</h4>
          <button class="btn" :disabled="store.busy" @click="startPairing">生成配对码</button>
          <div class="kvm-code" v-if="pairingCode">{{ pairingCode }}</div>
          <p class="muted" v-if="pairingCode">把上面 6 位码输入到另一台设备（5 分钟内有效）</p>
        </div>
        <div class="kvm-pair-col">
          <h4>这台设备输入码</h4>
          <div class="kvm-pair-row">
            <input
              v-model="joinCode"
              class="input kvm-code-input"
              inputmode="numeric"
              maxlength="6"
              placeholder="输入 6 位配对码"
              @keyup.enter="joinPairing"
            />
            <button class="btn primary" :disabled="store.busy || busyPairing || joinCode.length !== 6" @click="joinPairing">
              配对
            </button>
          </div>
          <p class="muted" v-if="devices && devices.discovered.length > 0">
            局域网发现：{{ devices.discovered.map((d) => d.name).join('、') }}
          </p>
          <p class="muted" v-else>对方设备需要运行本应用，配对码在生成端显示</p>
        </div>
      </div>

      <div v-if="devices && devices.peers.length > 0" class="kvm-peers">
        <div class="kvm-peer" v-for="p in devices.peers" :key="p.id">
          <div class="kvm-peer-main">
            <strong>{{ p.name }}</strong>
            <span class="tag" :title="`两台设备应显示相同指纹`">指纹 {{ p.fingerprint }}</span>
            <span class="tag">上次地址 {{ p.addr }}</span>
          </div>
          <div class="kvm-peer-side">
            <label class="muted">我的输入口</label>
            <select class="input" :value="p.my_source" @change="setSource(p.id, $event)">
              <option v-for="s in KVM_SOURCES" :key="s.value" :value="s.value">{{ s.label }}</option>
            </select>
            <button class="btn sm danger-ghost" :title="`解除与 ${p.name} 的配对`" @click="removePeer(p.id)">
              <Icon name="trash" /> 解除
            </button>
          </div>
        </div>
        <div class="kvm-unpair" v-if="devices.peers.length > 1">
          <button v-if="!confirmUnpair" class="btn sm danger-ghost" @click="confirmUnpair = true">解除所有配对</button>
          <template v-else>
            <span class="muted">确定解除全部 {{ devices.peers.length }} 台设备？</span>
            <button class="btn sm danger" @click="unpairAll">确认解除</button>
            <button class="btn sm" @click="confirmUnpair = false">取消</button>
          </template>
        </div>
      </div>
      <p class="muted" v-else>配对信息只存在本机（对端设备名、密钥指纹、上次地址），不上传任何服务器。</p>
    </Card>
  </div>
</template>
