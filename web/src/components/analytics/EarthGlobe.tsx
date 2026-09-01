// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useRef, useMemo, useCallback, useEffect, useState } from 'react'
import { Canvas, useFrame, useThree } from '@react-three/fiber'
import { OrbitControls, useTexture } from '@react-three/drei'
import * as THREE from 'three'
import type { VisitorInfo } from '@/api/client/types.gen'

// ─── Types ───────────────────────────────────────────────────────

interface EarthGlobeProps {
  visitors: VisitorInfo[]
  liveVisitorIds: Set<string>
  globeSize: number
  paused: boolean
  onProjectedMarkersUpdate: (markers: ProjectedMarker[]) => void
}

export interface ProjectedMarker {
  x: number
  y: number
  z: number // positive = front-facing
  visitor: VisitorInfo
}

// ─── Helpers ─────────────────────────────────────────────────────

/** Convert lat/lng (degrees) to a 3D position on a sphere of given radius */
function latLngToVector3(
  lat: number,
  lng: number,
  radius: number
): THREE.Vector3 {
  const phi = (90 - lat) * (Math.PI / 180)
  const theta = (lng + 180) * (Math.PI / 180)
  const x = -(radius * Math.sin(phi) * Math.cos(theta))
  const y = radius * Math.cos(phi)
  const z = radius * Math.sin(phi) * Math.sin(theta)
  return new THREE.Vector3(x, y, z)
}

// ─── Earth Sphere ────────────────────────────────────────────────

function Earth() {
  const meshRef = useRef<THREE.Mesh>(null)

  const [colorMap, bumpMap] = useTexture([
    '/earth-blue-marble.jpg',
    '/earth-topology.png',
  ])

  return (
    <mesh ref={meshRef}>
      <sphereGeometry args={[2, 64, 64]} />
      <meshPhongMaterial
        map={colorMap}
        bumpMap={bumpMap}
        bumpScale={0.03}
        specularMap={bumpMap}
        specular={new THREE.Color(0x333333)}
        shininess={12}
        emissive={new THREE.Color(0x112244)}
        emissiveIntensity={0.15}
      />
    </mesh>
  )
}

// ─── Atmosphere glow ─────────────────────────────────────────────

function Atmosphere() {
  const vertexShader = `
    varying vec3 vNormal;
    void main() {
      vNormal = normalize(normalMatrix * normal);
      gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
    }
  `

  const fragmentShader = `
    varying vec3 vNormal;
    void main() {
      float intensity = pow(0.65 - dot(vNormal, vec3(0.0, 0.0, 1.0)), 2.0);
      gl_FragColor = vec4(0.3, 0.6, 1.0, 1.0) * intensity * 0.7;
    }
  `

  return (
    <mesh>
      <sphereGeometry args={[2.12, 64, 64]} />
      <shaderMaterial
        vertexShader={vertexShader}
        fragmentShader={fragmentShader}
        blending={THREE.AdditiveBlending}
        side={THREE.BackSide}
        transparent
      />
    </mesh>
  )
}

// ─── Visitor markers (3D dots on globe surface) ──────────────────

interface MarkersProps {
  visitors: VisitorInfo[]
  liveVisitorIds: Set<string>
}

function VisitorMarkers({ visitors, liveVisitorIds }: MarkersProps) {
  const markersData = useMemo(() => {
    return visitors
      .filter((v) => v.latitude != null && v.longitude != null)
      .map((v) => {
        const pos = latLngToVector3(v.latitude!, v.longitude!, 2.01)
        const isLive = liveVisitorIds.has(v.visitor_id)
        return { position: pos, isLive, visitor: v }
      })
  }, [visitors, liveVisitorIds])

  return (
    <group>
      {markersData.map((m) => (
        <mesh key={m.visitor.id} position={m.position}>
          <sphereGeometry args={[m.isLive ? 0.03 : 0.018, 8, 8]} />
          <meshBasicMaterial
            color={m.isLive ? '#4ade80' : '#60a5fa'}
            transparent
            opacity={m.isLive ? 1 : 0.8}
          />
        </mesh>
      ))}
      {markersData
        .filter((m) => m.isLive)
        .map((m) => (
          <PulseRing
            key={`pulse-${m.visitor.id}`}
            position={m.position}
          />
        ))}
    </group>
  )
}

function PulseRing({ position }: { position: THREE.Vector3 }) {
  const ringRef = useRef<THREE.Mesh>(null)

  useFrame(({ clock }) => {
    if (!ringRef.current) return
    const t = (clock.getElapsedTime() % 2) / 2
    const scale = 1 + t * 2
    ringRef.current.scale.set(scale, scale, scale)
    const mat = ringRef.current.material as THREE.MeshBasicMaterial
    mat.opacity = 0.6 * (1 - t)
  })

  const normal = position.clone().normalize()
  const quaternion = new THREE.Quaternion()
  quaternion.setFromUnitVectors(new THREE.Vector3(0, 0, 1), normal)

  return (
    <mesh ref={ringRef} position={position} quaternion={quaternion}>
      <ringGeometry args={[0.03, 0.045, 16]} />
      <meshBasicMaterial
        color="#4ade80"
        transparent
        opacity={0.6}
        side={THREE.DoubleSide}
      />
    </mesh>
  )
}

// ─── Projection bridge: 3D → 2D for avatar overlays ─────────────

interface ProjectionBridgeProps {
  visitors: VisitorInfo[]
  onProjectedMarkersUpdate: (markers: ProjectedMarker[]) => void
  canvasWidth: number
  canvasHeight: number
}

function ProjectionBridge({
  visitors,
  onProjectedMarkersUpdate,
  canvasWidth,
  canvasHeight,
}: ProjectionBridgeProps) {
  const { camera } = useThree()
  const visitorsWithLocation = useMemo(
    () =>
      visitors.filter(
        (v) => v.latitude != null && v.longitude != null && !v.is_crawler
      ),
    [visitors]
  )

  useFrame(() => {
    if (canvasWidth === 0 || canvasHeight === 0) return

    const projected: ProjectedMarker[] = []

    for (const v of visitorsWithLocation) {
      const worldPos = latLngToVector3(v.latitude!, v.longitude!, 2.01)
      const ndc = worldPos.clone().project(camera)
      if (ndc.z > 1) continue

      const x = ((ndc.x + 1) / 2) * canvasWidth
      const y = ((-ndc.y + 1) / 2) * canvasHeight

      const camDir = new THREE.Vector3()
      camera.getWorldDirection(camDir)
      const pointDir = worldPos.clone().normalize()
      const dot = -pointDir.dot(camDir)

      if (dot > 0.05) {
        projected.push({ x, y, z: dot, visitor: v })
      }
    }

    onProjectedMarkersUpdate(projected)
  })

  return null
}

// ─── Controls with pause support ─────────────────────────────────

function GlobeControls({ paused }: { paused: boolean }) {
  return (
    <OrbitControls
      enableZoom
      enablePan={false}
      minDistance={3}
      maxDistance={8}
      autoRotate={!paused}
      autoRotateSpeed={0.15}
      enableDamping
      dampingFactor={0.05}
    />
  )
}

// ─── Scene ───────────────────────────────────────────────────────

interface SceneProps {
  visitors: VisitorInfo[]
  liveVisitorIds: Set<string>
  paused: boolean
  onProjectedMarkersUpdate: (markers: ProjectedMarker[]) => void
  canvasWidth: number
  canvasHeight: number
}

function Scene({
  visitors,
  liveVisitorIds,
  paused,
  onProjectedMarkersUpdate,
  canvasWidth,
  canvasHeight,
}: SceneProps) {
  return (
    <>
      <ambientLight intensity={0.55} />
      <hemisphereLight args={['#bbddff', '#334466', 0.4]} />
      <directionalLight position={[5, 3, 5]} intensity={1.4} color="#ffffff" />
      <directionalLight
        position={[-5, -2, -5]}
        intensity={0.35}
        color="#6699cc"
      />
      <directionalLight
        position={[0, 5, -3]}
        intensity={0.25}
        color="#ffffff"
      />

      <Earth />
      <Atmosphere />
      <VisitorMarkers visitors={visitors} liveVisitorIds={liveVisitorIds} />

      <ProjectionBridge
        visitors={visitors.slice(0, 30)}
        onProjectedMarkersUpdate={onProjectedMarkersUpdate}
        canvasWidth={canvasWidth}
        canvasHeight={canvasHeight}
      />

      <GlobeControls paused={paused} />
    </>
  )
}

// ─── Main component ──────────────────────────────────────────────

export function EarthGlobe({
  visitors,
  liveVisitorIds,
  globeSize,
  paused,
  onProjectedMarkersUpdate,
}: EarthGlobeProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const [dimensions, setDimensions] = useState({ width: 0, height: 0 })

  useEffect(() => {
    function updateDimensions() {
      if (containerRef.current) {
        const rect = containerRef.current.getBoundingClientRect()
        setDimensions({ width: rect.width, height: rect.height })
      }
    }
    updateDimensions()
    window.addEventListener('resize', updateDimensions)
    return () => window.removeEventListener('resize', updateDimensions)
  }, [])

  const stableCallback = useCallback(
    (markers: ProjectedMarker[]) => {
      onProjectedMarkersUpdate(markers)
    },
    [onProjectedMarkersUpdate]
  )

  return (
    <div
      ref={containerRef}
      className="w-full h-full"
      style={{ minHeight: globeSize || 500 }}
    >
      <Canvas
        camera={{ position: [0, 0, 4.5], fov: 45 }}
        style={{ background: 'transparent' }}
        gl={{ antialias: true, alpha: true }}
      >
        <Scene
          visitors={visitors}
          liveVisitorIds={liveVisitorIds}
          paused={paused}
          onProjectedMarkersUpdate={stableCallback}
          canvasWidth={dimensions.width}
          canvasHeight={dimensions.height}
        />
      </Canvas>
    </div>
  )
}
