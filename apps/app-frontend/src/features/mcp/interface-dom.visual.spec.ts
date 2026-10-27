import { afterEach, beforeEach, expect, it } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'

import { createMcpUiController } from './interface-dom'

let controller: ReturnType<typeof createMcpUiController>
let root: HTMLDivElement

beforeEach(async () => {
    const router = createRouter({
        history: createMemoryHistory(),
        routes: [
            { path: '/', component: { template: '<div />' } },
            { path: '/settings/mcp', component: { template: '<div />' } },
        ],
    })
    await router.push('/')
    root = document.createElement('div')
    root.style.cssText =
        'position:fixed;inset:0;background:white;z-index:999999;display:flex;flex-wrap:wrap;align-content:start;align-items:start;gap:8px'
    root.innerHTML =
        '<button>Launch test instance</button><input aria-label="Instance name" value="private-input-value"><input type="password" value="secret-password"><textarea aria-label="Notes">private-notes</textarea>'
    document.body.append(root)
    controller = createMcpUiController(router)
})

afterEach(() => {
    controller.stop()
    root.remove()
})

async function context(scope = 'owner') {
    return (await controller.execute({ kind: 'context' }, scope)) as {
        view_id: string
        elements: { element_id: string; label: string }[]
    }
}

it('observes route and controls without exposing values or passwords', async () => {
    const page = await context()
    expect(page.elements.some((element) => element.label === 'Launch test instance')).toBe(true)
    expect(JSON.stringify(page)).not.toContain('private-input-value')
    expect(JSON.stringify(page)).not.toContain('private-notes')
    expect(JSON.stringify(page)).not.toContain('secret-password')
    expect(page.elements.filter((element) => element.label === 'Notes')).toHaveLength(1)
})

it('runs normal click/input handlers and expires the view after each action', async () => {
    let clicked = 0
    root.querySelector('button')!.addEventListener('click', () => clicked++)
    let page = await context()
    const button = page.elements.find((element) => element.label === 'Launch test instance')!
    await controller.execute(
        { kind: 'click', view_id: page.view_id, element_id: button.element_id },
        'owner',
    )
    expect(clicked).toBe(1)
    await expect(
        controller.execute(
            { kind: 'click', view_id: page.view_id, element_id: button.element_id },
            'owner',
        ),
    ).rejects.toThrow('View expired')
    page = await context()
    const input = page.elements.find((element) => element.label === 'Instance name')!
    let changed = ''
    root.querySelector('input')!.addEventListener('input', (event) => {
        changed = (event.target as HTMLInputElement).value
    })
    await controller.execute(
        { kind: 'type', view_id: page.view_id, element_id: input.element_id, text: '新的实例' },
        'owner',
    )
    expect(changed).toBe('新的实例')
    page = await context()
    await controller.execute(
        { kind: 'type', view_id: page.view_id, element_id: input.element_id, text: '' },
        'owner',
    )
    expect(changed).toBe('')
})

it('rejects stale elements after movement or a different control session', async () => {
    const page = await context()
    const button = page.elements.find((element) => element.label === 'Launch test instance')!
    root.querySelector('button')!.style.transform = 'translateX(30px)'
    await expect(
        controller.execute(
            { kind: 'click', view_id: page.view_id, element_id: button.element_id },
            'owner',
        ),
    ).rejects.toThrow('Element moved')
    await expect(
        controller.execute(
            { kind: 'click', view_id: page.view_id, element_id: button.element_id },
            'another-session',
        ),
    ).rejects.toThrow('View expired')
})

it('refuses external/unknown navigation and reports a recognized route', async () => {
    for (const path of [
        'https://example.com',
        '//example.com',
        '/unknown',
        '/settings/mcp?token=secret',
        '/settings/mcp#fragment',
    ]) {
        await expect(controller.execute({ kind: 'navigate', path }, 'owner')).rejects.toThrow()
    }
    expect(await controller.execute({ kind: 'navigate', path: '/settings/mcp' }, 'owner')).toEqual({
        route: '/settings/mcp',
        reached: true,
    })
})

it('produces a real PNG capture with matching page context', async () => {
    root.style.background = 'rgb(34, 67, 123)'
    const screenshot = (await controller.execute({ kind: 'screenshot' }, 'owner')) as {
        image: { mime_type: string; data: string }
        context: { view_id: string }
    }
    expect(screenshot.image.mime_type).toBe('image/png')
    expect(screenshot.image.data.startsWith('iVBORw0KGgo')).toBe(true)
    expect(screenshot.context.view_id).toBeTruthy()
    const image = new Image()
    image.src = `data:image/png;base64,${screenshot.image.data}`
    await image.decode()
    const canvas = document.createElement('canvas')
    canvas.width = image.width
    canvas.height = image.height
    const ctx = canvas.getContext('2d')!
    ctx.drawImage(image, 0, 0)
    expect(Array.from(ctx.getImageData(10, image.height - 10, 1, 1).data)).toEqual([
        34, 67, 123, 255,
    ])
})

it('dispatches pointer activation before click for popup controls', async () => {
    const events: string[] = []
    const button = root.querySelector('button')!
    for (const name of ['pointerdown', 'pointerup', 'click'])
        button.addEventListener(name, () => events.push(name))
    const page = await context()
    const target = page.elements.find((element) => element.label === 'Launch test instance')!
    await controller.execute(
        { kind: 'click', view_id: page.view_id, element_id: target.element_id },
        'owner',
    )
    expect(events).toEqual(['pointerdown', 'pointerup', 'click'])
})

it('edits focused text using selection and deletion keys', async () => {
    const input = root.querySelector('input')!
    input.focus()
    let page = await context()
    await controller.execute(
        { kind: 'key', view_id: page.view_id, key: 'a', shift: false, control: true },
        'owner',
    )
    expect(input.selectionStart).toBe(0)
    expect(input.selectionEnd).toBe(input.value.length)
    page = await context()
    await controller.execute(
        { kind: 'key', view_id: page.view_id, key: 'Backspace', shift: false, control: false },
        'owner',
    )
    expect(input.value).toBe('')
})

it('scrolls the nearest container and invalidates its old view', async () => {
    root.innerHTML =
        '<div style="height:80px;overflow:auto"><button>Scroll target</button><div style="height:500px"></div></div>'
    const page = await context()
    const target = page.elements.find((element) => element.label === 'Scroll target')!
    await controller.execute(
        {
            kind: 'scroll',
            view_id: page.view_id,
            element_id: target.element_id,
            direction: 'down',
            pixels: 120,
        },
        'owner',
    )
    expect(root.firstElementChild!.scrollTop).toBe(120)
    await expect(
        controller.execute(
            { kind: 'click', view_id: page.view_id, element_id: target.element_id },
            'owner',
        ),
    ).rejects.toThrow('View expired')
})
