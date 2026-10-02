import { listen } from '@tauri-apps/api/event'
import { readonly, ref } from 'vue'

import { clear_official_login_marker, get_anti_piracy_status } from '@/helpers/auth'

export type AntiPiracyStatus = {
	region: 'checking' | 'cn' | 'non_cn' | 'unavailable'
	restricted: boolean
}

const status = ref<AntiPiracyStatus>({ region: 'checking', restricted: false })
let initialization: Promise<void> | undefined

export function isOfflineAccountRestrictedError(error: unknown): boolean {
	const message =
		error instanceof Error
			? error.message
			: typeof error === 'string'
				? error
				: JSON.stringify(error)
	return message?.includes('OFFLINE_ACCOUNT_RESTRICTED') ?? false
}

export async function refreshAntiPiracyStatus(): Promise<void> {
	status.value = (await get_anti_piracy_status()) as AntiPiracyStatus
}

export function useAntiPiracyStatus() {
	initialization ??= (async () => {
		try {
			await listen<AntiPiracyStatus>('anti-piracy-status-changed', ({ payload }) => {
				status.value = payload
			})
		} catch (error) {
			console.warn('Could not subscribe to offline account eligibility changes', error)
		}
		await refreshAntiPiracyStatus()
	})().catch((error) => {
		console.warn('Could not initialize offline account eligibility status', error)
	})
	return {
		status: readonly(status),
		refresh: refreshAntiPiracyStatus,
		clear: async () => {
			status.value = (await clear_official_login_marker()) as AntiPiracyStatus
		},
	}
}
