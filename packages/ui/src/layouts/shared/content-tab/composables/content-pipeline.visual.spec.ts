import { mount } from '@vue/test-utils'
import { expect, it, vi } from 'vitest'
import { defineComponent, h, nextTick, ref } from 'vue'

import type { ContentItem } from '../types'
import { useContentPipeline } from './content-pipeline'

vi.mock('#ui/composables/i18n', () => ({
    defineMessage: (message: unknown) => message,
    defineMessages: (messages: unknown) => messages,
    useVIntl: () => ({
        formatMessage: (message: { defaultMessage: string }) => message.defaultMessage,
    }),
}))

it('keeps an update filter through zero results and admits progressive candidates', async () => {
    const items = ref<ContentItem[]>([
        {
            id: 'one',
            file_name: 'one.jar',
            enabled: true,
            project_type: 'mod',
        },
    ])
    const ready = ref(false)
    let pipeline!: ReturnType<typeof useContentPipeline>
    const wrapper = mount(
        defineComponent({
            setup() {
                pipeline = useContentPipeline({
                    items,
                    duplicateItems: ref([]),
                    sortItems: (items) => items,
                    getItemId: (item) => item.id,
                    showUpdateFilter: true,
                    filterOptionsReady: ready,
                    initialFilters: { typeFilters: [], statusFilters: ['updates'] },
                })
                return () =>
                    h(
                        'div',
                        pipeline.filteredItems.value.map((item) => h('span', item.id)),
                    )
            },
        }),
    )
    expect(pipeline.filteredItems.value).toEqual([])
    ready.value = true
    await new Promise((resolve) => setTimeout(resolve, 300))
    expect(pipeline.selectedStatusFilters.value).toEqual(['updates'])
    expect(pipeline.row2FilterOptions.value.some((option) => option.id === 'updates')).toBe(true)
    items.value = [
        {
            ...items.value[0],
            resourceUpdate: {
                project: { source: 'modrinth', projectId: 'project' },
                releaseId: 'new',
            },
        },
    ]
    await nextTick()
    await new Promise((resolve) => setTimeout(resolve, 120))
    expect(wrapper.text()).toBe('one')
    expect(pipeline.filterCounts.value.updates).toBe(1)
    items.value = [{ ...items.value[0], resourceUpdate: null }]
    await nextTick()
    await new Promise((resolve) => setTimeout(resolve, 300))
    expect(wrapper.text()).toBe('')
    expect(pipeline.selectedStatusFilters.value).toEqual(['updates'])
    wrapper.unmount()
})

it('publishes counts during a continuous stream instead of waiting for a quiet period', async () => {
    vi.useFakeTimers()
    const items = ref<ContentItem[]>(
        Array.from({ length: 20 }, (_, index) => ({
            id: String(index),
            file_name: `${index}.jar`,
            enabled: true,
            project_type: 'mod',
        })),
    )
    let pipeline!: ReturnType<typeof useContentPipeline>
    const wrapper = mount(
        defineComponent({
            setup() {
                pipeline = useContentPipeline({
                    items,
                    duplicateItems: ref([]),
                    sortItems: (items) => items,
                    getItemId: (item) => item.id,
                    showUpdateFilter: true,
                    initialFilters: { typeFilters: [], statusFilters: ['updates'] },
                })
                return () => h('div', String(pipeline.filterCounts.value.updates ?? 0))
            },
        }),
    )
    try {
        for (let index = 0; index < 20; index++) {
            items.value = items.value.map((item, position) =>
                position === index
                    ? {
                          ...item,
                          resourceUpdate: {
                              project: { source: 'modrinth', projectId: item.id },
                              releaseId: 'new',
                          },
                      }
                    : item,
            )
            await nextTick()
            await vi.advanceTimersByTimeAsync(20)
            if (index === 7) {
                expect(pipeline.filterCounts.value.updates).toBeGreaterThan(0)
                expect(pipeline.filterCounts.value.updates).toBeLessThan(20)
                expect(Number(wrapper.text())).toBeGreaterThan(0)
            }
        }
        await vi.advanceTimersByTimeAsync(100)
        expect(pipeline.filterCounts.value.updates).toBe(20)
    } finally {
        wrapper.unmount()
        vi.useRealTimers()
    }
})
