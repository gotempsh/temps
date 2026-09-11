// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from '@playwright/test'

const services = [
  {id:101,name:'Shared Postgres',service_type:'postgres',status:'running'},
  {id:102,name:'Cache Redis',service_type:'redis',status:'running'},
  {id:103,name:'App MariaDB',service_type:'mariadb',status:'running'},
]
const providers = ['postgres','redis','mariadb','mongodb','s3'].map(service_type => ({service_type,display_name:service_type,description:`Create ${service_type}`,color:'#333333',icon_url:'/favicon.ico'}))

test('existing databases can be searched and linked, while creation keeps project context',async ({page}) => {
  const {projects} = await (await page.request.get('/api/projects')).json()
  const project = projects[0]
  expect(project).toBeTruthy()
  await page.route('**/api/external-services',route => route.fulfill({json:services}))
  let linked = false
  let fail = true
  await page.route(`**/api/external-services/projects/${project.id}`,route => route.fulfill({json:linked ? [{service:services[2]}] : []}))
  await page.route('**/api/external-services/providers/metadata',route => route.fulfill({json:providers}))
  await page.route('**/api/external-services/103/projects',route => {
    expect(route.request().postDataJSON()).toEqual({project_id:project.id,database_provisioning_mode:'project_environment'})
    if (fail) return route.fulfill({status:500,json:{detail:'Database link failed'}})
    linked = true
    return route.fulfill({json:{}})
  })
  await page.goto(`/projects/${project.slug}/storage`)
  const search = page.getByRole('textbox',{name:'Search existing databases'})
  await search.fill('mariadb')
  await expect(page.getByText('App MariaDB',{exact:true})).toBeVisible()
  await expect(page.getByText('Shared Postgres',{exact:true})).toHaveCount(0)
  await page.getByRole('button',{name:'Link',exact:true}).click()
  await page.getByRole('button',{name:'Link database',exact:true}).click()
  await expect(page.getByText('Failed to link service',{exact:true}).first()).toBeVisible()
  await expect(page.getByRole('button',{name:'Link database',exact:true})).toBeEnabled()
  fail = false
  await page.getByRole('button',{name:'Link database',exact:true}).click()
  await expect(page.getByRole('heading',{name:'Linked',exact:true})).toBeVisible()
  await page.getByRole('button',{name:/Create database/}).click()
  await expect(page.getByRole('menuitem')).toHaveCount(5)
  await page.getByRole('menuitem',{name:'redis Create redis'}).click()
  await expect(page).toHaveURL(new RegExp(`storage/create\\?type=redis&project_id=${project.id}`))
})

test('empty databases offer all supported types and load errors do not look empty',async ({page}) => {
  const {projects} = await (await page.request.get('/api/projects')).json()
  const project = projects[0]
  let failed = true
  await page.route('**/api/external-services',route => failed ? route.fulfill({status:500,json:{detail:'Database list unavailable'}}) : route.fulfill({json:[]}))
  await page.route(`**/api/external-services/projects/${project.id}`,route => route.fulfill({json:[]}))
  await page.route('**/api/external-services/providers/metadata',route => route.fulfill({json:providers}))
  await page.goto(`/projects/${project.slug}/storage`)
  await expect(page.getByText('Databases could not be loaded')).toBeVisible()
  await expect(page.getByRole('heading',{name:'Create your first database'})).toHaveCount(0)
  failed = false
  await page.getByRole('button',{name:'Retry databases'}).click()
  await expect(page.getByRole('heading',{name:'Create your first database'})).toBeVisible()
  const cards = page.getByRole('region',{name:'Create your first database'})
  await expect(cards.getByRole('link')).toHaveCount(5)
  await expect(page.getByRole('menu')).toHaveCount(0)
  await page.screenshot({path:'/tmp/temps-databases-empty-cards.png',fullPage:true})
  await cards.getByRole('link',{name:'Create mongodb',exact:true}).click()
  await expect(page).toHaveURL(new RegExp(`project_id=${project.id}`))
})

for (const mode of ['project', 'custom'] as const) {
  test(`linking persists ${mode} database selection`, async ({page}) => {
    const {projects} = await (await page.request.get('/api/projects')).json()
    const project = projects[0]
    let selection: Record<string, unknown> | undefined
    await page.route('**/api/external-services',route => route.fulfill({json:[services[0]]}))
    await page.route(`**/api/external-services/projects/${project.id}`,route => route.fulfill({json:selection ? [{service:services[0],...selection}] : []}))
    await page.route('**/api/external-services/101/projects',route => {
      selection = route.request().postDataJSON()
      return route.fulfill({json:{}})
    })
    await page.goto(`/projects/${project.slug}/storage`)
    await expect(page.getByRole('region',{name:'Create a new database'})).toBeVisible()
    await page.getByRole('button',{name:'Link',exact:true}).click()
    await page.getByRole('radio',{name:mode === 'custom' ? /Custom database/ : /Database per project/}).click()
    if (mode === 'custom') {
      await page.getByRole('button',{name:'Link database',exact:true}).click()
      await expect(page.getByLabel('Database name',{exact:true})).toHaveAttribute('aria-invalid','true')
      expect(selection).toBeUndefined()
      await page.getByLabel('Database name',{exact:true}).fill('shared_catalog')
      await page.screenshot({path:'/tmp/temps-database-link-modes.png',fullPage:true})
    }
    await page.getByRole('button',{name:'Link database',exact:true}).click()
    await expect(page.getByRole('dialog')).not.toBeVisible()
    expect(selection).toEqual({project_id:project.id,database_provisioning_mode:mode,...(mode === 'custom' ? {custom_database_name:'shared_catalog'} : {})})
    await expect(page.getByText(mode === 'custom' ? /Custom database/ : /Per project/).first()).toBeVisible()
  })
}
