<script lang="ts">
import type { SettingsDiffRow } from '@/helpers/deep-link-settings'

export type PrivilegedActionRequest = {
	event: 'UpdateSettings' | 'StopInstance'
	source: string
	rows?: SettingsDiffRow[]
	instanceId?: string
	processCount?: number
}
</script>

<script setup lang="ts">
import { Button, Checkbox, commonMessages, defineMessages, NewModal, useVIntl } from '@modrinth/ui'
import { computed, nextTick, ref } from 'vue'

const { formatMessage } = useVIntl()
const messages = defineMessages({
	title: {
		id: 'app.privileged-modal.title',
		defaultMessage: 'Privileged action request',
	},
	warning: {
		id: 'app.privileged-modal.warning',
		defaultMessage:
			'This link was sent from outside Axolotl Launcher. If you did not create it yourself, choose Cancel.',
	},
	dangerSettings: {
		id: 'app.privileged-modal.danger-settings',
		defaultMessage:
			'These changes can alter security-sensitive settings such as launch commands, directories and the network. Review every row before confirming.',
	},
	dangerStop: {
		id: 'app.privileged-modal.danger-stop',
		defaultMessage:
			'This force-stops the running game of instance {instance}. Unsaved world progress may be lost.',
	},
	linkLabel: { id: 'app.privileged-modal.link-label', defaultMessage: 'Requested link' },
	changesLabel: { id: 'app.privileged-modal.changes-label', defaultMessage: 'Setting changes' },
	columnSetting: { id: 'app.privileged-modal.column-setting', defaultMessage: 'Setting' },
	columnBefore: { id: 'app.privileged-modal.column-before', defaultMessage: 'Current value' },
	columnAfter: { id: 'app.privileged-modal.column-after', defaultMessage: 'New value' },
	processCount: {
		id: 'app.privileged-modal.process-count',
		defaultMessage: '{count, plural, one {# running process} other {# running processes}} will be stopped.',
	},
	countdown: {
		id: 'app.privileged-modal.countdown',
		defaultMessage: 'Confirm unlocks in {seconds}s',
	},
	acknowledge: {
		id: 'app.privileged-modal.acknowledge',
		defaultMessage: 'I have read the warning above and understand the risks',
	},
	confirm: { id: 'app.privileged-modal.confirm', defaultMessage: 'Confirm and apply' },
})

const modal = ref<InstanceType<typeof NewModal>>()
const cancelWrap = ref<HTMLElement>()
const request = ref<PrivilegedActionRequest>()
const acknowledged = ref(false)
const secondsLeft = ref(0)
const settled = ref(true)
let resolver: ((ok: boolean) => void) | undefined
let countdownTimer: ReturnType<typeof setInterval> | undefined

const title = computed(() => formatMessage(messages.title))
const confirmDisabled = computed(() => secondsLeft.value > 0 || !acknowledged.value)

function stopCountdown() {
	if (countdownTimer) {
		clearInterval(countdownTimer)
		countdownTimer = undefined
	}
}

function finish(ok: boolean) {
	if (settled.value) return
	settled.value = true
	stopCountdown()
	modal.value?.hide()
	const resolve = resolver
	resolver = undefined
	resolve?.(ok)
}

function run(next: PrivilegedActionRequest, resolve: (ok: boolean) => void) {
	request.value = next
	acknowledged.value = false
	secondsLeft.value = 10
	settled.value = false
	resolver = resolve
	stopCountdown()
	countdownTimer = setInterval(() => {
		secondsLeft.value -= 1
		if (secondsLeft.value <= 0) stopCountdown()
	}, 1000)
	modal.value?.show()
	void nextTick(() => cancelWrap.value?.querySelector('button')?.focus())
}

let chain: Promise<void> = Promise.resolve()

function requestAction(next: PrivilegedActionRequest): Promise<boolean> {
	return new Promise<boolean>((resolve) => {
		chain = chain.then(() => run(next, resolve))
	})
}

defineExpose({ request: requestAction })
</script>

<template>
	<NewModal
		ref="modal"
		:header="title"
		fade="danger"
		max-width="42rem"
		width="min(42rem, calc(95vw - 10rem))"
		:on-hide="() => finish(false)"
	>
		<div class="flex w-full flex-col gap-4">
			<p class="m-0 text-sm font-semibold text-[var(--color-red)]">
				{{ formatMessage(messages.warning) }}
			</p>
			<p v-if="request?.event === 'UpdateSettings'" class="m-0 text-sm text-[var(--color-text-primary)]">
				{{ formatMessage(messages.dangerSettings) }}
			</p>
			<p v-else-if="request" class="m-0 text-sm text-[var(--color-text-primary)]">
				{{ formatMessage(messages.dangerStop, { instance: request.instanceId ?? '' }) }}
			</p>
			<div class="flex flex-col gap-1">
				<span class="text-xs font-semibold text-[var(--color-text-tertiary)]">
					{{ formatMessage(messages.linkLabel) }}
				</span>
				<code class="break-all rounded-[var(--radius-sm)] bg-surface-2 p-2 text-xs">{{
					request?.source
				}}</code>
			</div>
			<div
				v-if="request?.event === 'UpdateSettings'"
				class="flex flex-col gap-1"
			>
				<span class="text-xs font-semibold text-[var(--color-text-tertiary)]">
					{{ formatMessage(messages.changesLabel) }}
				</span>
				<table class="w-full table-fixed border-collapse text-xs">
					<thead>
						<tr class="text-left text-[var(--color-text-tertiary)]">
							<th class="w-2/5 break-all p-1 font-semibold">
								{{ formatMessage(messages.columnSetting) }}
							</th>
							<th class="w-[30%] break-all p-1 font-semibold">
								{{ formatMessage(messages.columnBefore) }}
							</th>
							<th class="w-[30%] break-all p-1 font-semibold">
								{{ formatMessage(messages.columnAfter) }}
							</th>
						</tr>
					</thead>
					<tbody>
						<tr v-for="row in request?.rows ?? []" :key="row.key">
							<td class="break-all p-1 font-mono">{{ row.key }}</td>
							<td class="break-all p-1 text-[var(--color-text-tertiary)]">{{ row.before }}</td>
							<td class="break-all p-1 font-semibold">{{ row.after }}</td>
						</tr>
					</tbody>
				</table>
			</div>
			<p
				v-else-if="request?.processCount !== undefined"
				class="m-0 text-sm text-[var(--color-text-primary)]"
			>
				{{ formatMessage(messages.processCount, { count: request.processCount }) }}
			</p>
			<Checkbox v-model="acknowledged" :label="formatMessage(messages.acknowledge)" />
			<span class="text-xs text-[var(--color-text-tertiary)]">
				{{
					secondsLeft > 0
						? formatMessage(messages.countdown, { seconds: secondsLeft })
						: ''
				}}
			</span>
		</div>
		<template #actions>
			<div ref="cancelWrap" class="flex w-full flex-row justify-end gap-2">
				<Button @click="finish(false)">{{ formatMessage(commonMessages.cancelButton) }}</Button>
				<Button
					type="colored"
					color="red"
					:disabled="confirmDisabled"
					@click="finish(true)"
				>
					{{ formatMessage(messages.confirm) }}
				</Button>
			</div>
		</template>
	</NewModal>
</template>
