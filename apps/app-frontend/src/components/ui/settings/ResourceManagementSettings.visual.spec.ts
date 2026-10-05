import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, expect, it, vi } from 'vitest'
import { defineComponent, h, Suspense } from 'vue'

import ResourceManagementSettings from './ResourceManagementSettings.vue'

const fixture = vi.hoisted(() => ({ save: vi.fn(), handleError: vi.fn() }))

vi.mock('@/helpers/settings.ts', () => ({
	get: async () => ({
		doh_enabled: true,
		doh_server: 'https://doh.pub/dns-query',
		max_concurrent_downloads: 32,
		max_concurrent_writes: 4,
	}),
	set: fixture.save,
	getProxyConfig: async () => ({ mode: 'none', url: '', username: '', password: '' }),
	setProxyConfig: vi.fn(),
	testProxyConfig: vi.fn(),
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: async () => false }))
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }))
vi.mock('@/helpers/cache.js', () => ({ purge_cache_types: vi.fn() }))
vi.mock('@/helpers/curseforge', () => ({
	configureCurseForgeManualDownloadWatcher: async () => {},
}))
vi.mock('@/helpers/direct-link-sync', () => ({ syncConfiguredDirectLinks: async () => {} }))
vi.mock('@/helpers/downloads-scanner', () => ({
	getMissingContentScannerSettings: () => ({ enabled: false, directory: null }),
	setMissingContentScannerSettings: vi.fn(),
}))
vi.mock('@/helpers/utils.js', () => ({ showAppDbBackupsFolder: vi.fn() }))
vi.mock('@/store/state', () => ({ useTheming: () => ({ getFeatureFlag: () => false }) }))

vi.mock('@modrinth/ui', async () => {
	const { defineComponent, h } = await import('vue')
	const frame = defineComponent({
		setup:
			(_, { slots }) =>
			() =>
				h('div', slots.default?.()),
	})
	const model = (type: 'select' | 'text' | 'checkbox') =>
		defineComponent({
			props: ['modelValue', 'options', 'disabled'],
			emits: ['update:modelValue'],
			setup:
				(props, { emit }) =>
				() =>
					h(
						type === 'select' ? 'select' : 'input',
						{
							type: type === 'select' ? undefined : type,
							value: props.modelValue,
							checked: props.modelValue,
							disabled: props.disabled,
							onChange: (event: Event) =>
								emit(
									'update:modelValue',
									type === 'checkbox'
										? (event.target as HTMLInputElement).checked
										: (event.target as HTMLInputElement).value,
								),
							onInput:
								type === 'text'
									? (event: Event) =>
											emit('update:modelValue', (event.target as HTMLInputElement).value)
									: undefined,
						},
						type === 'select'
							? props.options?.map((option: { value: string; label: string }) =>
									h('option', { value: option.value }, option.label),
								)
							: undefined,
					),
		})
	return {
		Card: frame,
		ConfirmModal: frame,
		Slider: frame,
		Combobox: model('select'),
		Toggle: model('checkbox'),
		StyledInput: model('text'),
		IconButton: defineComponent({
			props: ['label', 'disabled', 'loading'],
			setup:
				(props, { slots }) =>
				() =>
					h(
						'button',
						{
							'aria-label': props.label,
							disabled: props.disabled || props.loading,
						},
						slots.default?.(),
					),
		}),
		defineMessages: (messages: unknown) => messages,
		commonMessages: { saveButton: { defaultMessage: 'Save' } },
		injectNotificationManager: () => ({ handleError: fixture.handleError }),
		useVIntl: () => ({
			formatMessage: (message: { defaultMessage: string }) => message.defaultMessage,
		}),
	}
})

beforeEach(() => {
	fixture.save.mockReset().mockResolvedValue(undefined)
	fixture.handleError.mockReset()
})

async function openSettings() {
	const wrapper = mount(
		defineComponent({
			setup: () => () => h(Suspense, null, { default: () => h(ResourceManagementSettings) }),
		}),
		{ global: { directives: { tooltip: () => {} } } },
	)
	await flushPromises()
	const selector = wrapper.findAll('select').find((select) => select.text().includes('Google DNS'))!
	await selector.setValue('custom')
	return wrapper
}

it('an invalid custom draft stays local and does not prevent disabling DoH', async () => {
	const wrapper = await openSettings()
	try {
		await wrapper.get('#doh-server').setValue('')
		await flushPromises()
		expect(fixture.save).not.toHaveBeenCalled()
		expect(wrapper.get('[aria-label="Save"]').attributes('disabled')).toBeDefined()
		expect(wrapper.get('#doh-server-error').text()).toBe('Enter a valid HTTPS URL.')
		await wrapper.get('#doh-enabled').setValue(false)
		await flushPromises()
		expect(fixture.save).toHaveBeenLastCalledWith(
			expect.objectContaining({
				doh_enabled: false,
				doh_server: 'https://doh.pub/dns-query',
			}),
		)
	} finally {
		wrapper.unmount()
	}
})

it('a valid custom URL takes effect only when saved', async () => {
	const wrapper = await openSettings()
	try {
		await wrapper.get('#doh-server').setValue('https://custom.example/dns-query')
		await flushPromises()
		expect(fixture.save).not.toHaveBeenCalled()
		await wrapper.get('[aria-label="Save"]').trigger('click')
		await flushPromises()
		expect(fixture.save).toHaveBeenCalledTimes(1)
		expect(fixture.save).toHaveBeenCalledWith(
			expect.objectContaining({ doh_server: 'https://custom.example/dns-query' }),
		)
	} finally {
		wrapper.unmount()
	}
})

it('a rejected save preserves the draft and restores the previous active URL', async () => {
	const wrapper = await openSettings()
	try {
		await wrapper.get('#doh-server').setValue('https://custom.example/dns-query')
		fixture.save.mockRejectedValueOnce(new Error('settings write failed'))
		await wrapper.get('[aria-label="Save"]').trigger('click')
		await flushPromises()
		expect((wrapper.get('#doh-server').element as HTMLInputElement).value).toBe(
			'https://custom.example/dns-query',
		)
		expect(fixture.handleError).toHaveBeenCalledTimes(1)
		expect(fixture.save).toHaveBeenLastCalledWith(
			expect.objectContaining({ doh_server: 'https://doh.pub/dns-query' }),
		)
	} finally {
		wrapper.unmount()
	}
})
