// ============================================================================
// Page transition animation component
// iOS-style slide transition effect
// ============================================================================

import { useEffect, useState, useRef } from 'react';

interface PageTransitionProps {
  children: React.ReactNode;
  pageKey: string;
  direction?: 'forward' | 'back';
}

export function PageTransition({ children, pageKey, direction = 'forward' }: PageTransitionProps) {
  const [displayChildren, setDisplayChildren] = useState(children);
  const [transitionStage, setTransitionStage] = useState<'idle' | 'exit' | 'enter'>('idle');
  const prevKeyRef = useRef(pageKey);

  useEffect(() => {
    if (pageKey !== prevKeyRef.current) {
      // Page switch, start the exit animation
      setTransitionStage('exit');
      
      const exitTimer = setTimeout(() => {
        // Exit animation finished, update content and start the enter animation
        setDisplayChildren(children);
        setTransitionStage('enter');
        prevKeyRef.current = pageKey;
        
        const enterTimer = setTimeout(() => {
          setTransitionStage('idle');
        }, 250);
        
        return () => clearTimeout(enterTimer);
      }, 200);
      
      return () => clearTimeout(exitTimer);
    } else {
      // Same page, just update the content
      setDisplayChildren(children);
    }
  }, [children, pageKey]);

  // Computes the animation class name based on direction and phase
  const getAnimationClass = () => {
    if (transitionStage === 'idle') return '';
    
    if (transitionStage === 'exit') {
      return direction === 'forward' 
        ? 'animate-slide-out-left' 
        : 'animate-slide-out-right';
    }
    
    if (transitionStage === 'enter') {
      return direction === 'forward' 
        ? 'animate-slide-in-right' 
        : 'animate-slide-in-left';
    }
    
    return '';
  };

  return (
    <div className={`w-full h-full ${getAnimationClass()}`}>
      {displayChildren}
    </div>
  );
}
