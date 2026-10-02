import { I18N_INJECTION_KEY } from '@modrinth/ui'
import { mount } from '@vue/test-utils'
import { expect, it, vi } from 'vitest'
import { nextTick, ref } from 'vue'

import en from '@/locales/en-US/index.json'
import zh from '@/locales/zh-CN/index.json'
import { applyTheme, waitFor } from '@/test/visual-harness'

import OfflineAccountRestrictionModal from './OfflineAccountRestrictionModal.vue'

const native = vi.hoisted(() => ({ openUrl: vi.fn() }))
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: native.openUrl }))

it.each([
	['en-US', en],
	['zh-CN', zh],
] as const)(
	'shows the restriction and working recovery actions in %s',
	async (locale, messages) => {
		applyTheme('dark')
		native.openUrl.mockReset()
		const wrapper = mount(OfflineAccountRestrictionModal, {
			attachTo: document.body,
			global: {
				provide: {
					[I18N_INJECTION_KEY as symbol]: {
						locale: ref(locale),
						t: (key: string) => messages[key as keyof typeof messages]?.message ?? key,
						setLocale: () => {},
					},
				},
				directives: { tooltip: () => {} },
			},
		})
		try {
			wrapper.vm.show()
			await nextTick()
			await waitFor(() => !!document.querySelector('[role="dialog"]'), {
				label: 'restriction dialog',
			})
			const dialog = document.querySelector('[role="dialog"]') as HTMLElement
			expect(dialog.textContent).toContain(
				messages['minecraft-account.restriction.description'].message,
			)
			expect(dialog.getBoundingClientRect().width).toBeGreaterThan(0)
			expect(dialog.scrollWidth).toBeLessThanOrEqual(dialog.clientWidth)
			const button = (key: 'buy' | 'sign-in') =>
				Array.from(dialog.querySelectorAll('button')).find((element) =>
					element.textContent?.includes(messages[`minecraft-account.restriction.${key}`].message),
				)!
			button('buy').click()
			expect(native.openUrl).toHaveBeenCalledWith(
				'https://www.minecraft.net/en-us/store/minecraft-java-bedrock-edition-pc',
			)
			button('sign-in').click()
			expect(wrapper.emitted('signIn')).toHaveLength(1)
		} finally {
			wrapper.unmount()
		}
	},
)
