<template>
    <NewModal ref="detectJavaModal" :header="formatMessage(messages.selectJavaVersion)">
        <div class="flex flex-col gap-4">
            <Table :columns="javaInstallColumns" :data="chosenInstallOptions" row-key="path">
                <template #cell-version="{ value }">
                    <span class="font-semibold text-[var(--color-text-default)]">{{ value }}</span>
                </template>
                <template #cell-path="{ value }">
                    <span v-tooltip="value" class="block truncate font-mono text-xs">{{
                        value
                    }}</span>
                </template>
                <template #cell-actions="{ row }">
                    <div class="flex items-center justify-end">
                        <Button
                            v-if="currentSelected.path === row.path"
                            class="!shadow-none"
                            disabled
                            ><CheckIcon /> {{ formatMessage(commonMessages.selectedLabel) }}
                        </Button>
                        <Button v-else class="!shadow-none" @click="setJavaInstall(row)"
                            ><PlusIcon /> {{ formatMessage(messages.select) }}
                        </Button>
                    </div>
                </template>
                <template #empty-state>
                    <div
                        v-if="loading"
                        class="flex items-center gap-2 p-4 text-[var(--color-text-tertiary)]"
                        role="status"
                    >
                        <SpinnerIcon class="size-4 animate-spin" />
                        {{ formatMessage(messages.searching) }}
                    </div>
                    <div v-else class="p-4 text-[var(--color-text-tertiary)]">
                        {{ formatMessage(messages.noneFound) }}
                    </div>
                </template>
            </Table>
            <div class="flex justify-end">
                <Button
                    type="outlined"
                    class="!shadow-none !border-surface-4 !border"
                    @click="$refs.detectJavaModal.hide()"
                    ><XIcon />
                    {{ formatMessage(commonMessages.cancelButton) }}
                </Button>
            </div>
        </div>
    </NewModal>
</template>
<script setup>
import { CheckIcon, PlusIcon, SpinnerIcon, XIcon } from '@modrinth/assets'
import {
    Button,
    commonMessages,
    defineMessages,
    injectNotificationManager,
    NewModal,
    Table,
    useVIntl,
} from '@modrinth/ui'
import { onUnmounted, ref } from 'vue'

import { trackEvent } from '@/helpers/analytics'
import { java_discovery_listener } from '@/helpers/events'
import { find_filtered_jres } from '@/helpers/jre.js'

const { handleError } = injectNotificationManager()
const { formatMessage } = useVIntl()
const messages = defineMessages({
    selectJavaVersion: {
        id: 'app.java.select-version',
        defaultMessage: 'Select Java version',
    },
    select: { id: 'app.java.select', defaultMessage: 'Select' },
    noneFound: {
        id: 'app.java.none-found',
        defaultMessage: 'No Java installations found!',
    },
    searching: {
        id: 'app.java.searching',
        defaultMessage: 'Looking for Java installations…',
    },
    version: { id: 'app.java.table.version', defaultMessage: 'Version' },
    path: { id: 'app.java.table.path', defaultMessage: 'Path' },
    actions: { id: 'app.java.table.actions', defaultMessage: 'Actions' },
})

const chosenInstallOptions = ref([])
const detectJavaModal = ref(null)
const currentSelected = ref({})
const javaInstallColumns = [
    { key: 'version', label: formatMessage(messages.version), width: '9rem' },
    { key: 'path', label: formatMessage(messages.path) },
    { key: 'actions', label: formatMessage(messages.actions), align: 'right', width: '10rem' },
]

const lastRequestedVersion = ref(null)
const loading = ref(false)
let unlistenJavaDiscovery = null
let listening = null
let disposed = false
// Each opening owns its results; a slower lookup for an earlier version must not replace them.
let generation = 0

defineExpose({
    show: async (version, currentSelectedJava) => {
        const request = ++generation
        lastRequestedVersion.value = version ?? null
        currentSelected.value = currentSelectedJava || { path: '', version: '' }
        chosenInstallOptions.value = []
        loading.value = true
        detectJavaModal.value.show()
        listening ??= java_discovery_listener(refreshInstallOptions).then((unlisten) => {
            if (disposed) unlisten()
            else unlistenJavaDiscovery = unlisten
        })
        await loadInstallOptions(request)
    },
})

async function loadInstallOptions(request, quiet = false) {
    try {
        const found = await find_filtered_jres(lastRequestedVersion.value, false, false)
        if (request === generation && found) chosenInstallOptions.value = found
    } catch (error) {
        if (request === generation && !quiet) handleError(error)
    } finally {
        if (request === generation) loading.value = false
    }
}

function refreshInstallOptions() {
    return loadInstallOptions(generation, true)
}

onUnmounted(() => {
    disposed = true
    generation++
    if (unlistenJavaDiscovery) {
        unlistenJavaDiscovery()
        unlistenJavaDiscovery = null
    }
})

const emit = defineEmits(['submit'])

function setJavaInstall(javaInstall) {
    emit('submit', javaInstall)
    detectJavaModal.value.hide()
    trackEvent('JavaAutoDetect', {
        path: javaInstall.path,
        version: javaInstall.version,
    })
}
</script>
