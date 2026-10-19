<script setup lang="ts">
// The reactions recorded on a message (M5 §3.6, §10), slice 5h2.
//
// Each chip is a (kind or emoji, sender) group of recorded rows whose
// state Rust derived (D93): "recorded (derived)", "cannot be determined"
// when a row is undated or two tie, and, for a sender that is not
// recorded, one chip per row ("recorded add", "recorded removal"). A
// removal hides its chip; the hidden ones are counted, not drawn. The list
// reads as text ("Reactions: Loved by You, recorded (derived)"). The rows
// behind a chip are listed in the details panel (slice 5h3), so in this
// slice a chip is not a control.
import { computed } from 'vue';
import type { HandleDto, ReactionChipDto, ReactionSummaryDto, SafeId } from '@/types';
import SafeText from './SafeText.vue';
import { reactionsView } from '@/services/messageView';
import { BUBBLE_TEXT, CHIP_SENDER_TEXT, CHIP_STATE_TEXT, REACTION_KIND_TEXT } from '@/services/viewerText';

const props = defineProps<{
  reactions: ReactionSummaryDto;
  handles: ReadonlyMap<SafeId, HandleDto>;
}>();

const view = computed(() => reactionsView(props.reactions));

function handleOf(chip: ReactionChipDto): HandleDto | null {
  return chip.sender.kind === 'handle' ? (props.handles.get(chip.sender.handle) ?? null) : null;
}

function stateClass(chip: ReactionChipDto): string {
  return chip.state === 'shown' ? 'oma-chip' : `oma-chip oma-chip-${chip.state}`;
}
</script>

<template>
  <div class="oma-reactions">
    <span class="visually-hidden">{{ BUBBLE_TEXT.reactionsHeading }}:</span>
    <ul v-if="view.chips.length" class="oma-chip-list">
      <li v-for="(chip, i) in view.chips" :key="i" :class="stateClass(chip)">
        <SafeText
          v-if="chip.kind === 'emoji' && chip.emoji && chip.emoji.kind === 'text'"
          :value="chip.emoji"
          quiet
        /><template v-else>{{ REACTION_KIND_TEXT[chip.kind] }}</template
        >{{ ' ' }}<span class="oma-chip-by"
          >{{ BUBBLE_TEXT.reactionBy }}{{ ' ' }}<template v-if="chip.sender.kind === 'handle'"
            ><SafeText v-if="handleOf(chip)" :value="handleOf(chip)!.address" mode="identifier" ltr quiet /><template
              v-else
              >{{ CHIP_SENDER_TEXT.handle }} ({{ BUBBLE_TEXT.addressNotLoaded }})</template
            ></template
          ><template v-else>{{ CHIP_SENDER_TEXT[chip.sender.kind] }}</template></span
        >{{ ', ' }}<span class="oma-chip-state">{{ CHIP_STATE_TEXT[chip.state] }}</span>
      </li>
    </ul>
    <p v-if="view.removed" class="oma-body-note">{{ view.removed }}</p>
    <p v-if="view.unpaired" class="oma-body-note">{{ view.unpaired }}</p>
    <p v-if="view.more" class="oma-body-note">{{ view.more }}</p>
    <p v-if="view.over" class="oma-body-note">{{ view.over }}</p>
  </div>
</template>
