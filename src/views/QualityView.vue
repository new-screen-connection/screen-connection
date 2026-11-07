<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue';
import type { DisplayInfo } from '../types';
import { invoke } from '../api/tauri';
import { parseHdrStates } from '../api/hdr';
import { act, offerAdminRelaunch, store } from '../store';
import Card from '../components/Card.vue';
import Icon from '../components/Icon.vue';

const COLOR_SPACES = [
  { v: 'sRGB', label: 'sRGB（网络 / 通用）' },
  { v: 'p3', label: 'Display P3（苹果设备）' },
  { v: 'adobergb', label: 'Adobe RGB（印刷 / 摄影）' },
  { v: 'dcip3', label: 'DCI-P3（影院 / 视频）' },
  { v: 'rec709', label: 'Rec.709（高清视频）' },
  { v: 'rec2020', label: 'Rec.2020（HDR / 超高清）' },
  { v: 'prophoto', label: 'ProPhoto RGB（后期广色域）' },
  { v: 'gray', label: '灰度 / 单色' },
];

const colorSpace = ref('sRGB');
const gamma = ref(100);
const systemVolume = ref(0);
const systemMute = ref(false);
const volumeAdjustable = ref(true);
const audioName = ref('');
const hdrLabel = ref('HDR 读取中');
const hdrOn = ref(false);
const mitv = ref(false);
const mitvVolume = ref(0);

const applyColor = (): void => {
  void act(
    () => invoke('apply_color_space', { space: colorSpace.value }),
    `已把全部屏幕对齐到 ${colorSpace.value}`,
  ).then(offerAdminRelaunch);
};

const matchMac = (): void => {
  void act(() => invoke('display_action', { action: 'match-mac', displayId: '', targetId: null, value: null }), '已对齐 Mac 内建屏色彩（Display P3）').then(
    offerAdminRelaunch,
  );
};

const syncBrightness = (): void => {
  void act(async () => {
    const ds = (await invoke<DisplayInfo[]>('get_displays')) ?? [];
    const targets = ds.filter((d) => d.ddc && d.brightness !== null);
    const max = Math.max(0, ...targets.map((d) => d.brightness ?? 0));
    for (const d of targets) {
      await invoke('set_brightness', { displayId: d.id, value: max });
    }
    return `已同步 ${targets.length} 台显示器亮度到 ${max}%`;
  }, (r) => r);
};

const toggleHdr = (): void => {
  void act(async () => {
    const msg = await invoke<string>('hdr_set', { on: !hdrOn.value });
    await refreshHdr(); // 回读真实状态：API 说成功 != 屏幕真的切了（错误 87 案例）
    return msg;
  }, (r) => String(r));
};

const commitGamma = (): void => {
  void act(() => invoke('gamma_set', { pct: gamma.value }), `总亮度已调整到 ${gamma.value}%`);
};

const resetGamma = (): void => {
  gamma.value = 100;
  void act(() => invoke('gamma_set', { pct: 100 }), '总亮度已复位到 100%（原始曲线）');
};

const mute = (): void => {
  void act(async () => {
    await invoke('set_system_volume', { value: systemMute.value ? systemVolume.value : 0 });
    systemMute.value = !systemMute.value;
  }, () => (systemMute.value ? '已取消静音' : '已静音'));
};

const commitSystemVolume = (): void => {
  void act(() => invoke('set_system_volume', { value: systemVolume.value }), `系统音量已调整到 ${systemVolume.value}%`);
};

const commitMitvVolume = (): void => {
  void act(() => invoke('mitv_volume_set', { target: mitvVolume.value }), `小米屏音量已调整到 ${mitvVolume.value}%`);
};

const refreshHdr = async (): Promise<void> => {
  const raw = String(await invoke<string>('hdr_states').catch(() => ''));
  const { supported, on } = parseHdrStates(raw);
  hdrOn.value = on > 0;
  hdrLabel.value = !raw || supported === 0 ? 'HDR 不可用' : on > 0 ? `HDR 已开启（${on}/${supported} 台）` : 'HDR 未开启';
};

onMounted(async () => {
  const [ga, ep] = await Promise.all([
    invoke<string>('gamma_get').catch(() => '100'),
    invoke<{ id: string; name: string; volume: number; mute: boolean; adjustable: boolean; selected: boolean; is_default: boolean }[]>('audio_endpoints').catch(() => []),
  ]);
  gamma.value = Number(ga) || 100;
  const sel = ep.find((e) => e.selected) ?? ep.find((e) => e.is_default);
  if (sel) {
    audioName.value = sel.name;
    systemVolume.value = sel.volume;
    systemMute.value = sel.mute;
    volumeAdjustable.value = sel.adjustable;
  }
  const raw = String(await invoke<string>('hdr_states').catch(() => ''));
  const { supported, on } = parseHdrStates(raw);
  hdrOn.value = on > 0;
  hdrLabel.value = !raw || supported === 0 ? 'HDR 不可用' : on > 0 ? `HDR 已开启（${on}/${supported} 台）` : 'HDR 未开启';
  mitv.value = Boolean(await invoke<boolean>('mitv_available').catch(() => false));
  if (mitv.value) {
    mitvVolume.value = Number(await invoke<number>('mitv_volume_get').catch(() => 0));
  }

  // 用户可能在 Windows 设置里开关 HDR（本程序对部分机型只能读不能写），
  // 窗口重新获得焦点（从设置页切回来）时重读一次状态，按钮别再显示旧状态。
  window.addEventListener('focus', refreshHdr);
});

onUnmounted(() => {
  window.removeEventListener('focus', refreshHdr);
});
</script>

<template>
  <div class="view">
    <div class="view-head">
      <h1 class="view-title">画质</h1>
      <span class="view-sub">色彩、HDR 与亮度音量</span>
    </div>

    <Card title="色彩同步" icon="palette">
      <div class="field">
        <span>目标色彩空间</span>
        <select v-model="colorSpace">
          <option v-for="c in COLOR_SPACES" :key="c.v" :value="c.v">{{ c.label }}</option>
        </select>
      </div>
      <div class="btn-grid" style="margin-top: 12px">
        <button class="btn primary" :disabled="store.busy" @click="applyColor">应用到所有屏幕</button>
        <button class="btn" :disabled="store.busy" @click="matchMac">对齐 Mac 内建屏</button>
      </div>
    </Card>

    <Card title="亮度与 HDR" icon="bright">
      <div class="slider-row">
        <label>总亮度</label>
        <input type="range" min="40" max="160" v-model.number="gamma" @change="commitGamma" />
        <span class="val">{{ gamma }}%</span>
      </div>
      <div class="btn-grid">
        <button class="btn" :disabled="store.busy" @click="syncBrightness">
          <Icon name="bright" :size="13" />亮度同步
        </button>
        <button class="btn" :disabled="store.busy || hdrLabel === 'HDR 不可用'" @click="toggleHdr">
          <Icon name="hdr" :size="13" />{{ hdrLabel }}
        </button>
        <button class="btn" :disabled="store.busy" @click="resetGamma">亮度复位</button>
      </div>
    </Card>

    <Card title="系统音量" icon="volume">
      <template #actions>
        <span v-if="audioName" class="tag">{{ audioName }}</span>
      </template>
      <div class="slider-row">
        <label>音量</label>
        <input type="range" min="0" max="100" v-model.number="systemVolume" :disabled="!volumeAdjustable" @change="commitSystemVolume" />
        <span class="val">{{ systemVolume }}%</span>
      </div>
      <p v-if="!volumeAdjustable" class="status-line">当前端点为固定音量设备，请用显示器 DDC 音量滑块控制。</p>
      <div class="slider-row" v-if="mitv">
        <label>小米音量</label>
        <input type="range" min="0" max="100" v-model.number="mitvVolume" @change="commitMitvVolume" />
        <span class="val">{{ mitvVolume }}%</span>
      </div>
      <div class="btn-grid">
        <button class="btn" :disabled="store.busy" @click="mute">{{ systemMute ? '取消静音' : '静音' }}</button>
      </div>
    </Card>
  </div>
</template>
